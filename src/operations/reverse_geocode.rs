#![deny(warnings)]

// Reverse geocoding: resolve latitude/longitude to a location name via Photon (Komoot)
// https://photon.komoot.io — powered by OpenStreetMap data

use crate::error::{GeocodeError, Result};
use serde::Deserialize;
use serde_json::Value;

pub(crate) const PHOTON_REVERSE_URL: &str = "https://photon.komoot.io/reverse";

#[derive(Debug, Deserialize)]
struct PhotonResponse {
    features: Vec<PhotonFeature>,
}

#[derive(Debug, Deserialize)]
struct PhotonFeature {
    properties: PhotonProperties,
    geometry: PhotonGeometry,
}

#[derive(Debug, Deserialize)]
struct PhotonProperties {
    name: Option<String>,
    country: Option<String>,
    countrycode: Option<String>,
    state: Option<String>,
    #[serde(rename = "type")]
    place_type: Option<String>,
}

#[derive(Debug, Deserialize)]
struct PhotonGeometry {
    coordinates: Vec<f64>,
}

/// Reverse-geocode a latitude/longitude pair and return the nearest matching location.
pub async fn reverse_geocode_location(
    client: &reqwest::Client,
    latitude: f64,
    longitude: f64,
    language: Option<&str>,
) -> Result<Value> {
    reverse_geocode_location_with_base(client, PHOTON_REVERSE_URL, latitude, longitude, language)
        .await
}

/// Reverse-geocode against a configurable base URL (used in tests to point at httpmock).
pub async fn reverse_geocode_location_with_base(
    client: &reqwest::Client,
    base_url: &str,
    latitude: f64,
    longitude: f64,
    language: Option<&str>,
) -> Result<Value> {
    let language = language.unwrap_or("en");

    log_reverse_geocode_request(latitude, longitude);

    let lat_str = latitude.to_string();
    let lon_str = longitude.to_string();

    let response = client
        .get(base_url)
        .query(&[
            ("lat", lat_str.as_str()),
            ("lon", lon_str.as_str()),
            ("lang", language),
        ])
        .header(
            "User-Agent",
            concat!("geocode-mcp/", env!("CARGO_PKG_VERSION")),
        )
        .send()
        .await?;

    let status = response.status();
    if !status.is_success() {
        return Err(GeocodeError::ApiError(format!(
            "Photon reverse API returned HTTP {}",
            status.as_u16()
        ))
        .into());
    }

    let resp = response.json::<PhotonResponse>().await?;

    if resp.features.is_empty() {
        return Err(GeocodeError::LocationNotFound(format!(
            "No location found for coordinates: {}, {}",
            latitude, longitude
        ))
        .into());
    }

    // Return the first (nearest) feature
    let f = resp
        .features
        .into_iter()
        .next()
        .expect("non-empty checked above");

    // Photon coordinates are [longitude, latitude] (GeoJSON order)
    let coords = &f.geometry.coordinates;
    if coords.len() < 2 {
        return Err(GeocodeError::ApiError(
            "Photon reverse API returned feature without coordinates".to_string(),
        )
        .into());
    }

    let result = serde_json::json!({
        "name": f.properties.name,
        "latitude": coords[1],
        "longitude": coords[0],
        "country": f.properties.country,
        "country_code": f.properties.countrycode,
        "region": f.properties.state,
        "type": f.properties.place_type,
    });

    Ok(result)
}

/// Log that an outbound reverse-geocode lookup is starting: geocode-mcp's
/// call to the upstream Photon API for a coordinate pair.
///
/// A coordinate pair is a tool argument -- content, never an id -- so it
/// stays at DEBUG and is never attached to a span (a span field would leave
/// the process with `otel` on regardless of level). Kept as its own function
/// so a test can drive it directly, without a network call.
fn log_reverse_geocode_request(latitude: f64, longitude: f64) {
    tracing::debug!(
        latitude = %latitude,
        longitude = %longitude,
        "requesting reverse geocode from photon"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    /// AC (mcp-core#40): each outbound Photon lookup logs a `debug!` event
    /// naming the coordinate pair, so an operator debugging a slow or failed
    /// reverse geocode has something to correlate against -- and only at
    /// DEBUG, because a coordinate pair is content (D10).
    #[test]
    fn log_reverse_geocode_request_puts_the_coordinates_at_debug_only() {
        use std::collections::BTreeMap;
        use std::sync::{Arc, Mutex};
        use tracing::field::{Field, Visit};
        use tracing_subscriber::Layer;
        use tracing_subscriber::layer::{Context, SubscriberExt};

        type LoggedEvent = (tracing::Level, BTreeMap<String, String>);

        #[derive(Clone, Default)]
        struct Capture(Arc<Mutex<Vec<LoggedEvent>>>);

        struct Collector<'a>(&'a mut BTreeMap<String, String>);
        impl Visit for Collector<'_> {
            fn record_str(&mut self, field: &Field, value: &str) {
                self.0.insert(field.name().to_string(), value.to_string());
            }
            fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
                self.0
                    .insert(field.name().to_string(), format!("{value:?}"));
            }
        }

        impl<S: tracing::Subscriber> Layer<S> for Capture {
            fn on_event(&self, event: &tracing::Event<'_>, _ctx: Context<'_, S>) {
                let mut fields = BTreeMap::new();
                event.record(&mut Collector(&mut fields));
                self.0
                    .lock()
                    .expect("capture lock is only held to push one record")
                    .push((*event.metadata().level(), fields));
            }
        }

        const SENTINEL_LATITUDE: f64 = 12.345678;
        const SENTINEL_LONGITUDE: f64 = -98.765432;
        let capture = Capture::default();
        let subscriber = tracing_subscriber::registry().with(capture.clone());
        tracing::subscriber::with_default(subscriber, || {
            log_reverse_geocode_request(SENTINEL_LATITUDE, SENTINEL_LONGITUDE);
        });

        let events = capture
            .0
            .lock()
            .expect("capture lock is only held to push one record");
        assert_eq!(
            events.len(),
            1,
            "requesting a reverse geocode lookup must log exactly one event: {events:?}"
        );
        let (level, fields) = &events[0];
        assert_eq!(
            *level,
            tracing::Level::DEBUG,
            "the outbound request must log at DEBUG, so it stays off the INFO band"
        );
        assert_eq!(
            fields.get("latitude").map(String::as_str),
            Some(SENTINEL_LATITUDE.to_string()).as_deref(),
            "the event must carry the latitude that was looked up"
        );
        assert_eq!(
            fields.get("longitude").map(String::as_str),
            Some(SENTINEL_LONGITUDE.to_string()).as_deref(),
            "the event must carry the longitude that was looked up"
        );
    }
}
