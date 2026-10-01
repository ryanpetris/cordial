use cordial_core::compact::{Preference, Record};
use cordial_core::model::{
    hidpp::{FeatureId, FeatureRevision},
    settings::*,
};
use cordial_core::settings::*;

#[derive(Default)]
struct Store {
    saved: Vec<Preference>,
    writes: usize,
    fail: bool,
}
impl PreferenceStore for Store {
    async fn replace(&mut self, values: &[Preference]) -> Result<(), Error> {
        self.saved = values.to_vec();
        Ok(())
    }
    async fn remove_all(&mut self) -> Result<(), Error> {
        if self.fail {
            return Err(Error::Storage);
        }
        self.writes += 1;
        self.saved.clear();
        Ok(())
    }
    async fn save(&mut self, preference: &Preference) -> Result<(), Error> {
        if self.fail {
            return Err(Error::Storage);
        }
        self.writes += 1;
        self.saved
            .retain(|s| s.metadata.key != preference.metadata.key);
        self.saved.push(preference.clone());
        Ok(())
    }
    async fn remove(&mut self, key: SettingKey) -> Result<(), Error> {
        if self.fail {
            return Err(Error::Storage);
        }
        self.writes += 1;
        self.saved.retain(|s| s.metadata.key != key);
        Ok(())
    }
}
fn setting() -> Record {
    Record::from_wire(&Setting {
        key: SettingKey::BacklightEnabled,
        kind: SettingType::Bool,
        writable: true,
        feature: FeatureId::BACKLIGHT,
        feature_version: FeatureRevision(3),
        scope: SettingScope::Device,
        choices: vec![],
        min: None,
        max: None,
        step: None,
        managed: false,
        desired: SettingValue::Null,
        observed: SettingValue::Bool(true),
        fresh: true,
        observed_at_ms: Some(42),
        observation_source: Some(ObservationSource::Read),
        state: SettingState::Unmanaged,
        error: None,
    })
    .unwrap()
}
#[test]
fn snapshots_keep_values_through_observation_forget_and_unpair() {
    embassy_futures::block_on(async {
        let mut catalog = Catalog::default();
        let mut store = Store::default();
        let key = SettingKey::BacklightEnabled;
        catalog.connection(true, true);
        catalog.replace_discovery(vec![setting()], vec![]).unwrap();
        catalog
            .set(key, SettingValue::Bool(false), &mut store)
            .await
            .unwrap();
        let saved = catalog.snapshot().unwrap();
        let before = saved[0].wire();
        assert_eq!(catalog.take_changed().count(), 1);
        assert_eq!(catalog.take_changed().count(), 0);
        catalog
            .observe(key, SettingValue::Bool(false), 50, ObservationSource::Event)
            .unwrap();
        assert_eq!(saved[0].wire(), before);
        assert_eq!(catalog.records()[0].differs_from_saved(), Some(false));
        catalog.forget(key, &mut store).await.unwrap();
        assert!(catalog.preferences().next().is_none());
        catalog.unpair(&mut store).await.unwrap();
        assert!(catalog.records().is_empty());
        assert_eq!(saved[0].wire(), before);
    });
}
#[test]
fn disabled_edits_are_saved_only_events_do_not_change_preferences_and_forget_is_offline() {
    embassy_futures::block_on(async {
        let mut catalog = Catalog::default();
        let mut store = Store::default();
        catalog.connection(true, true);
        catalog.replace_discovery(vec![setting()], vec![]).unwrap();
        assert_eq!(catalog.preferences().count(), 0);
        catalog.connection(false, false);
        assert_eq!(
            catalog
                .set(
                    SettingKey::BacklightEnabled,
                    SettingValue::Bool(false),
                    &mut store
                )
                .await,
            Err(Error::NotConnected)
        );
        catalog.connection(true, false);
        catalog.replace_discovery(vec![setting()], vec![]).unwrap();
        assert!(catalog.can_refresh());
        assert!(!catalog.can_apply());
        assert_eq!(
            catalog
                .set(
                    SettingKey::BacklightEnabled,
                    SettingValue::Bool(false),
                    &mut store
                )
                .await,
            Ok(Saved::SavedOnly)
        );
        assert_eq!(store.writes, 1);
        assert_eq!(store.saved[0].metadata.key, SettingKey::BacklightEnabled);
        catalog
            .observe(
                SettingKey::BacklightEnabled,
                SettingValue::Bool(true),
                43,
                ObservationSource::Event,
            )
            .unwrap();
        assert_eq!(catalog.records()[0].differs_from_saved(), Some(true));
        assert_eq!(store.writes, 1);
        catalog.connection(false, false);
        assert_eq!(catalog.records()[0].differs_from_saved(), None);
        catalog
            .forget(SettingKey::BacklightEnabled, &mut store)
            .await
            .unwrap();
        assert_eq!(catalog.records().len(), 1);
        assert!(catalog.preferences().count() == 0);
        assert_eq!(catalog.preferences().count(), 0);
        assert!(store.saved.is_empty());
        catalog.unpair(&mut store).await.unwrap();
        assert!(catalog.records().is_empty());
        assert!(catalog.features().is_empty());
    });
}
#[test]
fn failed_saves_and_busy_device_leave_preferences_unchanged() {
    embassy_futures::block_on(async {
        let mut catalog = Catalog::default();
        let mut store = Store::default();
        catalog.connection(true, true);
        catalog.replace_discovery(vec![setting()], vec![]).unwrap();
        store.fail = true;
        assert_eq!(
            catalog
                .set(
                    SettingKey::BacklightEnabled,
                    SettingValue::Bool(false),
                    &mut store
                )
                .await,
            Err(Error::Storage)
        );
        assert!(catalog.records()[0].preference.is_none());
        assert!(catalog.preferences().count() == 0);
        assert_eq!(catalog.preferences().count(), 0);
        store.fail = false;
        catalog
            .set(
                SettingKey::BacklightEnabled,
                SettingValue::Bool(false),
                &mut store,
            )
            .await
            .unwrap();
        catalog.set_busy(true);
        assert_eq!(
            catalog
                .set(
                    SettingKey::BacklightEnabled,
                    SettingValue::Bool(true),
                    &mut store
                )
                .await,
            Err(Error::Busy)
        );
        assert_eq!(
            catalog
                .forget(SettingKey::BacklightEnabled, &mut store)
                .await,
            Err(Error::Busy)
        );
        assert_eq!(store.writes, 1);
        catalog.set_busy(false);
        store.fail = true;
        assert_eq!(
            catalog
                .forget(SettingKey::BacklightEnabled, &mut store)
                .await,
            Err(Error::Storage)
        );
        assert_eq!(catalog.preferences().next().unwrap().value, 0);
        assert!(catalog.records()[0].preference.is_some());
    });
}

#[test]
fn forget_survives_capability_changes_and_keeps_cached_rows() {
    embassy_futures::block_on(async {
        let mut catalog = Catalog::default();
        let mut store = Store::default();
        catalog.connection(true, true);
        catalog.replace_discovery(vec![setting()], vec![]).unwrap();
        catalog
            .set(
                SettingKey::BacklightEnabled,
                SettingValue::Bool(false),
                &mut store,
            )
            .await
            .unwrap();
        let mut read_only = setting();
        read_only.writable = false;
        catalog.replace_discovery(vec![read_only], vec![]).unwrap();
        assert!(catalog.records()[0].writable);
        assert_eq!(catalog.records()[0].state, SettingState::Unsupported);
        catalog.connection(false, false);
        catalog
            .forget(SettingKey::BacklightEnabled, &mut store)
            .await
            .unwrap();
        assert!(store.saved.is_empty());
        assert!(catalog.preferences().count() == 0);

        catalog.connection(true, true);
        catalog.replace_discovery(vec![setting()], vec![]).unwrap();
        catalog
            .set(
                SettingKey::BacklightEnabled,
                SettingValue::Bool(false),
                &mut store,
            )
            .await
            .unwrap();
        catalog
            .forget(SettingKey::BacklightEnabled, &mut store)
            .await
            .unwrap();
        catalog.replace_discovery(vec![], vec![]).unwrap();
        assert_eq!(catalog.records().len(), 1);
        assert!(!catalog.records()[0].fresh);
        assert_eq!(
            catalog
                .set(
                    SettingKey::BacklightEnabled,
                    SettingValue::Bool(true),
                    &mut store
                )
                .await,
            Err(Error::UnsupportedSetting)
        );
        catalog
            .forget(SettingKey::BacklightEnabled, &mut store)
            .await
            .unwrap();
        catalog.unpair(&mut store).await.unwrap();
        assert!(catalog.records().is_empty());
    });
}

#[test]
fn explicit_save_updates_metadata_and_disconnect_invalidates_applied_state() {
    embassy_futures::block_on(async {
        let mut catalog = Catalog::default();
        let mut store = Store::default();
        catalog.connection(true, true);
        catalog.replace_discovery(vec![setting()], vec![]).unwrap();
        catalog
            .set(
                SettingKey::BacklightEnabled,
                SettingValue::Bool(false),
                &mut store,
            )
            .await
            .unwrap();
        catalog
            .observe(
                SettingKey::BacklightEnabled,
                SettingValue::Bool(false),
                100,
                ObservationSource::Read,
            )
            .unwrap();
        assert_eq!(catalog.records()[0].state, SettingState::Applied);
        catalog.connection(false, true);
        assert_eq!(catalog.records()[0].state, SettingState::Pending);
        assert!(!catalog.records()[0].fresh);
        catalog.connection(true, true);
        let mut next = setting();
        next.metadata.revision = FeatureRevision(4);
        catalog.replace_discovery(vec![next], vec![]).unwrap();
        assert_eq!(store.writes, 1);
        store.fail = true;
        assert_eq!(
            catalog
                .set(
                    SettingKey::BacklightEnabled,
                    SettingValue::Bool(false),
                    &mut store
                )
                .await,
            Err(Error::Storage)
        );
        assert_eq!(
            catalog.preferences().next().unwrap().metadata.revision,
            FeatureRevision(3)
        );
        store.fail = false;
        catalog
            .set(
                SettingKey::BacklightEnabled,
                SettingValue::Bool(false),
                &mut store,
            )
            .await
            .unwrap();
        assert_eq!(store.saved[0].metadata.revision, FeatureRevision(4));
        assert_eq!(store.writes, 2);
        assert_eq!(
            catalog
                .set(
                    SettingKey::BacklightEnabled,
                    SettingValue::Bool(false),
                    &mut store
                )
                .await,
            Ok(Saved::Apply)
        );
        assert_eq!(store.writes, 2);
    });
}

#[test]
fn stored_metadata_and_observations_are_validated_without_device_writes() {
    embassy_futures::block_on(async {
        let mut catalog = Catalog::default();
        let mut store = Store::default();
        catalog.connection(true, true);
        assert_eq!(
            catalog
                .forget(SettingKey::BacklightEnabled, &mut store)
                .await,
            Err(Error::SettingsUnavailable)
        );
        catalog.replace_discovery(vec![setting()], vec![]).unwrap();
        catalog
            .set(
                SettingKey::BacklightEnabled,
                SettingValue::Bool(true),
                &mut store,
            )
            .await
            .unwrap();
        let saved = catalog.preferences().cloned().collect::<Vec<_>>();
        catalog
            .observe(
                SettingKey::BacklightEnabled,
                SettingValue::Bool(false),
                101,
                ObservationSource::Read,
            )
            .unwrap();
        assert_eq!(catalog.records()[0].source, Some(ObservationSource::Read));
        assert_eq!(catalog.records()[0].state, SettingState::ChangedOnDevice);
        assert_eq!(
            catalog.observe(
                SettingKey::BacklightEnabled,
                SettingValue::Null,
                102,
                ObservationSource::Read
            ),
            Err(Error::InvalidValue)
        );
        assert_eq!(
            catalog.observe(
                SettingKey::BacklightEnabled,
                SettingValue::Integer(1),
                102,
                ObservationSource::Event
            ),
            Err(Error::InvalidValue)
        );
        catalog.connection(true, false);
        assert!(!catalog.records()[0].fresh);
        assert_eq!(catalog.records()[0].state, SettingState::Pending);
        assert_eq!(store.writes, 1);

        let mut restored = Catalog::default();
        restored.restore_preferences(saved.clone()).unwrap();
        let mut invalid_bool = saved.clone();
        invalid_bool[0].value = 2;
        assert_eq!(
            restored.restore_preferences(invalid_bool),
            Err(Error::SettingsUnavailable)
        );
        assert_eq!(restored.preferences().next().unwrap().value, 1);
        let mut wrong_range = saved.clone();
        wrong_range[0].metadata.range = Some(cordial_core::compact::Range {
            min: 1,
            max: 2,
            step: 1,
        });
        assert_eq!(
            restored.restore_preferences(wrong_range),
            Err(Error::SettingsUnavailable)
        );
        let mut invalid = saved;
        invalid[0].metadata.key = SettingKey::WheelInfo;
        assert_eq!(
            restored.restore_preferences(invalid),
            Err(Error::SettingsUnavailable)
        );
        assert_eq!(
            restored.preferences().next().unwrap().metadata.key,
            SettingKey::BacklightEnabled
        );
        restored.connection(true, true);
        assert_eq!(
            restored
                .set(
                    SettingKey::BacklightEnabled,
                    SettingValue::Bool(false),
                    &mut store
                )
                .await,
            Err(Error::SettingsUnavailable)
        );
        let mut incompatible = setting();
        incompatible.metadata.scope = SettingScope::CurrentHost;
        let mut fresh = Catalog::default();
        fresh.connection(true, false);
        fresh.replace_discovery(vec![incompatible], vec![]).unwrap();
        let writes = store.writes;
        assert_eq!(
            fresh
                .set(
                    SettingKey::BacklightEnabled,
                    SettingValue::Bool(true),
                    &mut store
                )
                .await,
            Err(Error::UnsupportedSetting)
        );
        assert_eq!(store.writes, writes);
    });
}
