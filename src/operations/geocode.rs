#![deny(warnings)]

// Geocoding: resolve location name to latitude/longitude using Photon (Komoot) geocoding API
// https://photon.komoot.io — powered by OpenStreetMap data

use crate::error::{GeocodeError, Result};
use serde::Deserialize;
use serde_json::Value;

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
    let count = count.clamp(1, 10);
    let language = language.unwrap_or("en");

    let url = format!(
        "https://photon.komoot.io/api/?q={}&limit={}&lang={}",
        urlencoding(name),
        count,
        language
    );

    let resp = client
        .get(&url)
        .header("User-Agent", "geocode-mcp/0.1.0")
        .send()
        .await?
        .json::<PhotonResponse>()
        .await?;

    if resp.features.is_empty() {
        return Err(GeocodeError::LocationNotFound(format!(
            "No locations found for: {}",
            name
        ))
        .into());
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
        return Err(GeocodeError::LocationNotFound(format!(
            "No locations found for: {}",
            name
        ))
        .into());
    }

    Ok(serde_json::json!(locations))
}

fn urlencoding(s: &str) -> String {
    s.chars()
        .flat_map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' || c == '~' {
                vec![c]
            } else {
                format!("%{:02X}", c as u32).chars().collect()
            }
        })
        .collect()
}
