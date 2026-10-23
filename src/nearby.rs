//! What's around: MapKit points of interest near a location.

// MKMapItem.placemark is deprecated in favour of MKMapItem.location, which only
// exists on macOS 26+; the placemark works on every version MapKit's POI search does.
#![allow(deprecated)]

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use block2::RcBlock;
use objc2::AllocAnyThread;
use objc2::rc::Retained;
use objc2_core_location::CLLocation;
use objc2_foundation::NSError;
use objc2_foundation::NSArray;
use objc2_map_kit::*;
use serde::Serialize;

use crate::location::pump_until;
use crate::ui::Progress;

#[derive(Serialize)]
pub struct Poi {
    pub name: String,
    pub category: Option<String>,
    pub distance_m: f64,
    pub latitude: f64,
    pub longitude: f64,
    pub phone: Option<String>,
    pub url: Option<String>,
    /// Probably inside the building you're in, or one listing many tenants,
    /// rather than a place to walk to.
    pub in_building: bool,
}

/// "MKPOICategoryPublicTransport" → "Public transport", "MKPOICategoryEVCharger" → "EV charger"
fn category_name(raw: &str) -> String {
    let raw = raw.strip_prefix("MKPOICategory").unwrap_or(raw);
    let chars: Vec<char> = raw.chars().collect();
    let mut words: Vec<String> = Vec::new();
    for (i, &c) in chars.iter().enumerate() {
        // A new word starts at an uppercase letter that follows a lowercase one,
        // or that ends an acronym ("EVCharger" → "EV", "Charger")
        let starts_word = i > 0
            && c.is_uppercase()
            && (chars[i - 1].is_lowercase() || chars.get(i + 1).is_some_and(|n| n.is_lowercase()));
        if starts_word || words.is_empty() {
            words.push(String::new());
        }
        words.last_mut().expect("word pushed").push(c);
    }
    words
        .iter()
        .enumerate()
        .map(|(i, w)| {
            let acronym = w.len() > 1 && w.chars().all(char::is_uppercase);
            if i == 0 || acronym { w.clone() } else { w.to_lowercase() }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn poi_from(item: &MKMapItem, here: &CLLocation) -> Option<Poi> {
    unsafe {
        let name = item.name()?.to_string();
        let location = item.placemark().location()?;
        let coord = location.coordinate();
        Some(Poi {
            name,
            category: item.pointOfInterestCategory().map(|c| category_name(&c.to_string())),
            distance_m: location.distanceFromLocation(here),
            latitude: coord.latitude,
            longitude: coord.longitude,
            phone: item.phoneNumber().map(|p| p.to_string()),
            url: item.url().and_then(|u| u.absoluteString()).map(|u| u.to_string()),
            in_building: false,
        })
    }
}

/// Categories that count as "what's around": places you'd walk to, not mailboxes.
fn venue_categories() -> Vec<&'static MKPointOfInterestCategory> {
    unsafe {
        [
        MKPointOfInterestCategoryAirport,
        MKPointOfInterestCategoryAmusementPark,
        MKPointOfInterestCategoryAquarium,
        MKPointOfInterestCategoryATM,
        MKPointOfInterestCategoryBakery,
        MKPointOfInterestCategoryBank,
        MKPointOfInterestCategoryBeach,
        MKPointOfInterestCategoryBeauty,
        MKPointOfInterestCategoryBrewery,
        MKPointOfInterestCategoryCafe,
        MKPointOfInterestCategoryCampground,
        MKPointOfInterestCategoryCarRental,
        MKPointOfInterestCategoryCastle,
        MKPointOfInterestCategoryConventionCenter,
        MKPointOfInterestCategoryDistillery,
        MKPointOfInterestCategoryEVCharger,
        MKPointOfInterestCategoryFireStation,
        MKPointOfInterestCategoryFitnessCenter,
        MKPointOfInterestCategoryFoodMarket,
        MKPointOfInterestCategoryGasStation,
        MKPointOfInterestCategoryHospital,
        MKPointOfInterestCategoryHotel,
        MKPointOfInterestCategoryLandmark,
        MKPointOfInterestCategoryLaundry,
        MKPointOfInterestCategoryLibrary,
        MKPointOfInterestCategoryMarina,
        MKPointOfInterestCategoryMovieTheater,
        MKPointOfInterestCategoryMuseum,
        MKPointOfInterestCategoryMusicVenue,
        MKPointOfInterestCategoryNationalMonument,
        MKPointOfInterestCategoryNationalPark,
        MKPointOfInterestCategoryNightlife,
        MKPointOfInterestCategoryPark,
        MKPointOfInterestCategoryParking,
        MKPointOfInterestCategoryPharmacy,
        MKPointOfInterestCategoryPolice,
        MKPointOfInterestCategoryPostOffice,
        MKPointOfInterestCategoryPublicTransport,
        MKPointOfInterestCategoryRestaurant,
        MKPointOfInterestCategoryRestroom,
        MKPointOfInterestCategorySchool,
        MKPointOfInterestCategorySpa,
        MKPointOfInterestCategoryStadium,
        MKPointOfInterestCategoryStore,
        MKPointOfInterestCategoryTheater,
        MKPointOfInterestCategoryUniversity,
        MKPointOfInterestCategoryWinery,
        MKPointOfInterestCategoryZoo,
        ]
    }
    .into_iter()
    .flatten()
    .collect()
}

/// MapKit returns at most this many items per request.
pub const RESULT_CAP: usize = 50;

/// Venues around `here` (cafés, shops, transit, ...), nearest first. Businesses
/// that are only registered at an address have no category and are left out.
pub fn venues(here: &CLLocation, radius: f64, timeout: Duration, progress: &mut Progress) -> Result<Vec<Poi>, String> {
    progress.set("Looking around…");
    // filterIncludingAllCategories still returns uncategorized listings, which can fill
    // the whole result cap in an office building; an explicit list excludes them.
    let filter = unsafe {
        MKPointOfInterestFilter::initIncludingCategories(
            MKPointOfInterestFilter::alloc(),
            &NSArray::from_slice(&venue_categories()),
        )
    };
    let mut pois = request(here, radius, Some(&filter), timeout, progress)?;
    pois.retain(|p| p.category.is_some());
    Ok(pois)
}

/// Everything listed within `radius` of `here`, categorized or not. Dozens of
/// uncategorized entries at one spot usually means a shared office building.
pub fn listed_here(here: &CLLocation, radius: f64, timeout: Duration, progress: &mut Progress) -> Result<Vec<Poi>, String> {
    progress.set("Checking the building…");
    request(here, radius, None, timeout, progress)
}

/// Points of interest within `radius` metres, nearest first.
fn request(
    here: &CLLocation,
    radius: f64,
    filter: Option<&MKPointOfInterestFilter>,
    timeout: Duration,
    progress: &mut Progress,
) -> Result<Vec<Poi>, String> {
    type Outcome = Result<Vec<Retained<MKMapItem>>, String>;
    let outcome: Rc<RefCell<Option<Outcome>>> = Rc::default();
    let slot = outcome.clone();
    let handler = RcBlock::new(move |response: *mut MKLocalSearchResponse, error: *mut NSError| {
        let result = match (unsafe { response.as_ref() }, unsafe { error.as_ref() }) {
            (Some(r), _) => Ok(unsafe { r.mapItems() }.to_vec()),
            // MKErrorPlacemarkNotFound (4): nothing in range
            (None, Some(e)) if e.code() == 4 => Ok(Vec::new()),
            (None, Some(e)) => Err(format!("{} (MKError {})", e.localizedDescription(), e.code())),
            (None, None) => Ok(Vec::new()),
        };
        *slot.borrow_mut() = Some(result);
    });

    let search = unsafe {
        let request = MKLocalPointsOfInterestRequest::initWithCenterCoordinate_radius(
            MKLocalPointsOfInterestRequest::alloc(),
            here.coordinate(),
            radius,
        );
        request.setPointOfInterestFilter(filter);
        let search = MKLocalSearch::initWithPointsOfInterestRequest(MKLocalSearch::alloc(), &request);
        search.startWithCompletionHandler(RcBlock::as_ptr(&handler));
        search
    };
    if !pump_until(timeout, progress, || outcome.borrow().is_some()) {
        unsafe { search.cancel() };
        return Err(format!("timed out after {}s", timeout.as_secs_f64()));
    }
    let items = outcome.borrow_mut().take().expect("outcome set")?;
    let mut pois: Vec<Poi> = items.iter().filter_map(|i| poi_from(i, here)).collect();
    pois.sort_by(|a, b| a.distance_m.total_cmp(&b.distance_m));
    Ok(pois)
}

/// Listings sharing this many identical coordinates are one building's tenants.
const CLUSTER_MIN: usize = 3;

/// Flag listings that are tenants of one building: several at exactly the same
/// coordinates, or (when `here` looks like a shared office) anything within `here_radius`.
pub fn mark_in_building(pois: &mut [Poi], shared_office: bool, here_radius: f64) {
    // Registered addresses geocode to the same point; compare at ~1 m precision
    let key = |p: &Poi| ((p.latitude * 1e5).round() as i64, (p.longitude * 1e5).round() as i64);
    let keys: Vec<_> = pois.iter().map(key).collect();
    for (i, p) in pois.iter_mut().enumerate() {
        let clustered = keys.iter().filter(|k| **k == keys[i]).count() >= CLUSTER_MIN;
        p.in_building = clustered || (shared_office && p.distance_m <= here_radius);
    }
}

#[cfg(test)]
mod tests {
    use super::category_name;

    #[test]
    fn humanizes_categories() {
        assert_eq!(category_name("MKPOICategoryCafe"), "Cafe");
        assert_eq!(category_name("MKPOICategoryPublicTransport"), "Public transport");
        assert_eq!(category_name("MKPOICategoryEVCharger"), "EV charger");
        assert_eq!(category_name("MKPOICategoryATM"), "ATM");
    }
}
