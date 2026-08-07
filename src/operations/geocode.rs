#![deny(warnings)]

// Geocoding: resolve location name to latitude/longitude using Photon (Komoot) geocoding API
// https://photon.komoot.io — powered by OpenStreetMap data

use crate::error::{GeocodeError, Result};
use serde::Deserialize;
use serde_json::Value;

const PHOTON_API_URL: &str = "https://photon.komoot.io/api/";

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

/// Geocode a location name and return matching results.
pub async fn geocode_location(
    client: &reqwest::Client,
    name: &str,
    count: u32,
    language: Option<&str>,
) -> Result<Value> {
    geocode_location_with_base(client, PHOTON_API_URL, name, count, language).await
}

/// Geocode against a configurable base URL (used in tests to point at httpmock).
pub async fn geocode_location_with_base(
    client: &reqwest::Client,
    base_url: &str,
    name: &str,
    count: u32,
    language: Option<&str>,
) -> Result<Value> {
    let count = count.clamp(1, 10);
    let language = language.unwrap_or("en");

    let response = client
        .get(base_url)
        .query(&[
            ("q", name),
            ("limit", &count.to_string()),
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
            "Photon API returned HTTP {}",
            status.as_u16()
        ))
        .into());
    }

    let resp = response.json::<PhotonResponse>().await?;

    if resp.features.is_empty() {
        return Err(
            GeocodeError::LocationNotFound(format!("No locations found for: {}", name)).into(),
        );
    }

    let locations: Vec<Value> = resp
        .features
        .into_iter()
        .filter_map(|f| {
            // Photon coordinates are [longitude, latitude] (GeoJSON order)
            let coords = &f.geometry.coordinates;
            if coords.len() < 2 {
                return None;
            }
            let longitude = coords[0];
            let latitude = coords[1];

            Some(serde_json::json!({
                "name": f.properties.name,
                "latitude": latitude,
                "longitude": longitude,
                "country": f.properties.country,
                "country_code": f.properties.countrycode,
                "region": f.properties.state,
                "type": f.properties.place_type,
            }))
        })
        .collect();

    if locations.is_empty() {
        return Err(
            GeocodeError::LocationNotFound(format!("No locations found for: {}", name)).into(),
        );
    }

    Ok(serde_json::json!(locations))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// AC (mcp-core#40): each outbound Photon lookup logs a `debug!` event
    /// naming the place name, so an operator debugging a slow or failed
    /// geocode has something to correlate against -- and only at DEBUG,
    /// because a place name is content (D10).
    #[test]
    fn log_geocode_request_puts_the_name_at_debug_only() {
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

        const SENTINEL: &str = "MARKER-9f3d1c2a-sentinel-address";
        let capture = Capture::default();
        let subscriber = tracing_subscriber::registry().with(capture.clone());
        tracing::subscriber::with_default(subscriber, || {
            log_geocode_request(SENTINEL);
        });

        let events = capture
            .0
            .lock()
            .expect("capture lock is only held to push one record");
        assert_eq!(
            events.len(),
            1,
            "requesting a geocode lookup must log exactly one event: {events:?}"
        );
        let (level, fields) = &events[0];
        assert_eq!(
            *level,
            tracing::Level::DEBUG,
            "the outbound request must log at DEBUG, so it stays off the INFO band"
        );
        assert_eq!(
            fields.get("name").map(String::as_str),
            Some(SENTINEL),
            "the event must carry the name that was looked up"
        );
    }
}
