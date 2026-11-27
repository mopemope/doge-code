use dgc_daemon::provider::ProviderEvent;
use dgc_protocol::event::EventKind;
use serde_json::Value;

pub fn rate_limited_details(events: &[ProviderEvent]) -> Vec<&Value> {
    events
        .iter()
        .filter_map(|event| match event {
            ProviderEvent::Event(EventKind::RateLimited { detail }) => Some(detail),
            _ => None,
        })
        .collect()
}

#[track_caller]
pub fn assert_no_rate_limited(events: &[ProviderEvent]) {
    let details = rate_limited_details(events);
    assert!(details.is_empty(), "unexpected RateLimited: {details:?}");
}
