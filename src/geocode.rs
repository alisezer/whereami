//! Reverse geocoding: location → street address, area and time zone.

// CLGeocoder and CLPlacemark are deprecated in favour of MapKit's
// MKReverseGeocodingRequest, but they still work and avoid pulling in the MapKit bindings.
#![allow(deprecated)]

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use block2::RcBlock;
use objc2::rc::Retained;
use objc2_contacts::{CNPostalAddressFormatter, CNPostalAddressFormatterStyle};
use objc2_core_location::{CLGeocoder, CLLocation, CLPlacemark};
use objc2_foundation::{NSArray, NSError, NSString};
use serde::Serialize;

use crate::location::pump_until;
use crate::ui::Progress;

#[derive(Serialize)]
pub struct Place {
    /// The full address as it's written locally, one line per element.
    pub address: Option<String>,
    /// CoreLocation's short name: usually "Street 12", sometimes a landmark.
    pub name: Option<String>,
    pub street: Option<String>,
    pub house_number: Option<String>,
    pub neighbourhood: Option<String>,
    pub postal_code: Option<String>,
    pub city: Option<String>,
    pub district: Option<String>,
    pub region: Option<String>,
    pub country: Option<String>,
    pub country_code: Option<String>,
    pub points_of_interest: Vec<String>,
    pub timezone: Option<TimeZone>,
}

#[derive(Serialize)]
pub struct TimeZone {
    pub id: String,
    pub abbreviation: Option<String>,
    pub utc_offset_seconds: isize,
}

impl Place {
    /// The street line, built from parts when the placemark has no usable name.
    pub fn street_line(&self) -> Option<String> {
        match (&self.street, &self.house_number) {
            (Some(s), Some(n)) => Some(format!("{s} {n}")),
            (Some(s), None) => Some(s.clone()),
            _ => self.name.clone(),
        }
    }
}

fn string(s: Option<Retained<NSString>>) -> Option<String> {
    s.map(|s| s.to_string()).filter(|s| !s.trim().is_empty())
}

fn place_from(p: &CLPlacemark) -> Place {
    unsafe {
        let address = p
            .postalAddress()
            .map(|a| {
                CNPostalAddressFormatter::stringFromPostalAddress_style(
                    &a,
                    CNPostalAddressFormatterStyle::MailingAddress,
                )
                .to_string()
            })
            .filter(|s| !s.trim().is_empty());
        let points_of_interest = p
            .areasOfInterest()
            .map(|a| a.iter().map(|s| s.to_string()).collect())
            .unwrap_or_default();
        let timezone = p.timeZone().map(|tz| TimeZone {
            id: tz.name().to_string(),
            abbreviation: tz.abbreviation().map(|a| a.to_string()),
            utc_offset_seconds: tz.secondsFromGMT(),
        });
        Place {
            address,
            name: string(p.name()),
            street: string(p.thoroughfare()),
            house_number: string(p.subThoroughfare()),
            neighbourhood: string(p.subLocality()),
            postal_code: string(p.postalCode()),
            city: string(p.locality()),
            district: string(p.subAdministrativeArea()),
            region: string(p.administrativeArea()),
            country: string(p.country()),
            country_code: string(p.ISOcountryCode()),
            points_of_interest,
            timezone,
        }
    }
}

pub fn reverse_geocode(
    location: &CLLocation,
    timeout: Duration,
    progress: &mut Progress,
) -> Result<Place, String> {
    type Outcome = Result<Place, String>;
    let outcome: Rc<RefCell<Option<Outcome>>> = Rc::default();
    let slot = outcome.clone();
    let handler = RcBlock::new(move |placemarks: *mut NSArray<CLPlacemark>, error: *mut NSError| {
        let placemark = unsafe { placemarks.as_ref() }.and_then(|p| p.firstObject());
        let result = match (placemark, unsafe { error.as_ref() }) {
            (Some(p), _) => Ok(place_from(&p)),
            (None, Some(e)) => Err(format!("{} (CLError {})", e.localizedDescription(), e.code())),
            (None, None) => Err("no results".to_string()),
        };
        *slot.borrow_mut() = Some(result);
    });

    progress.set("Looking up address…");
    let geocoder = unsafe { CLGeocoder::new() };
    unsafe {
        geocoder.reverseGeocodeLocation_completionHandler(location, RcBlock::as_ptr(&handler));
    }
    if !pump_until(timeout, progress, || outcome.borrow().is_some()) {
        unsafe { geocoder.cancelGeocode() };
        return Err(format!("timed out after {}s", timeout.as_secs_f64()));
    }
    outcome.borrow_mut().take().expect("outcome set")
}
