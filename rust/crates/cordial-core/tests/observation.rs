use cordial_core::model::{
    errors::ErrorCode,
    hidpp::{FeatureId, FeatureRevision},
    settings::*,
};
use cordial_core::{
    compact::{Metadata, Observed, Preference, Record},
    settings::Catalog,
};

#[test]
fn new_observations_keep_an_unsupported_saved_choice_visible() {
    let original = Metadata {
        key: SettingKey::BacklightEffect,
        feature: FeatureId::BACKLIGHT,
        revision: FeatureRevision(3),
        scope: SettingScope::Device,
        choices: Box::new([0, 1, 2]),
        range: None,
    };
    let mut catalog = Catalog::default();
    catalog
        .restore_preferences(vec![Preference {
            metadata: original.clone(),
            value: 2,
        }])
        .unwrap();
    catalog.connection(true, true);
    let mut limited = original;
    limited.choices = Box::new([0, 1]);
    catalog
        .replace_discovery(vec![Record::new(limited, true)], vec![])
        .unwrap();
    assert_eq!(catalog.records()[0].state, SettingState::Unsupported);
    for value in ["static", "breathing"] {
        catalog
            .observe(
                SettingKey::BacklightEffect,
                SettingValue::Text(value.into()),
                100,
                ObservationSource::Event,
            )
            .unwrap();
        let r = &catalog.records()[0];
        assert_eq!(r.state, SettingState::Unsupported);
        assert_eq!(r.error, Some(ErrorCode::UnsupportedSetting));
        assert!(r.fresh);
        assert_eq!(r.source, Some(ObservationSource::Event));
        assert_ne!(r.observed, Observed::Missing);
        assert_eq!(r.preference.as_ref().unwrap().value, 2);
    }
}
