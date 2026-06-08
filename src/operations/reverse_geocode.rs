#![deny(warnings)]

// Reverse geocoding: resolve latitude/longitude to a location name via Photon (Komoot)
// https://photon.komoot.io — powered by OpenStreetMap data

use crate::error::{GeocodeError, Result};
use serde::Deserialize;
use serde_json::Value;

const PHOTON_REVERSE_URL: &str = "https://photon.komoot.io/reverse";

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
