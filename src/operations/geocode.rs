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
