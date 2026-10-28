use super::{Action, App, Dialog, Fleet, Kind, Page, Spot};
use crate::controller::Command;
impl<F: Fleet> App<F> {
    /// Presses a control.
    pub(crate) fn action(&mut self, action: Action) {
        if self.quitting {
            return;
        }
        if self.editing.is_some() && !matches!(action, Action::SettingStep(..)) {
            self.commit_editing();
        }
        match &action {
            Action::Quit => return self.quit(),
            Action::Help => {
                self.menu = None;
                self.dialog = Some(Dialog::Help);
                return;
            }
            Action::RefreshAdapters => return self.refresh_adapters(),
            Action::AddDevice => return self.open_add(),
            Action::AddDeviceTo(id) => return self.open_add_to(id),
            Action::Open(page) | Action::Show(page) => {
                self.menu = None;
                return self.open(page.clone());
            }
            Action::ConnectAdapter(id) => {
                self.menu = None;
                return self.connect_adapter(id);
            }
            Action::DisconnectAdapter(id) => {
                self.menu = None;
                return self.disconnect_adapter(id);
            }
            Action::Migrate(id) => return self.migrate_adapter(id),
            Action::CancelMigration => return self.cancel_migration(),
            Action::RenameAdapter(id) => {
                self.menu = None;
                let page = Page::Adapter(id.clone());
                if self.shown() != page {
                    self.open(page);
                }
                return self.open_rename(id);
            }
            Action::Tab(t) => {
                self.tab = *t;
                return;
            }
            Action::Field => {
                return;
            }
            Action::Cancel => return self.cancel(),
            Action::Submit => return self.submit(),
            Action::Confirm => return self.confirm(),
            Action::ResetName => {
                if let Some(Dialog::Rename(id)) = self.dialog.clone() {
                    self.submit_rename(&id, true);
                }
                return;
            }
            Action::Pick(id) => {
                if let Some(Dialog::Pick { chosen, .. }) = &mut self.dialog {
                    *chosen = *id;
                }
                return;
            }
            Action::PickPage(next) => {
                if let Some(Dialog::Pick { adapter, .. }) = self.dialog.clone() {
                    self.step_page(&adapter, true, *next);
                }
                return;
            }
            Action::PickNewProfile => return self.pick_new_profile(),
            Action::PickRetry => {
                if let Some(Dialog::Pick { adapter, .. }) = self.dialog.clone()
                    && self.picker_retryable(&adapter)
                {
                    self.read_page(&adapter, true, false);
                }
                return;
            }
            Action::ProfileDeleteConfirm(plan) => {
                if let Some(Dialog::ProfileDelete(a, id)) = self.dialog.clone()
                    && id == plan.profile
                {
                    self.delete_profile(&a, plan);
                }
                return;
            }
            _ => {}
        }
        if self.dialog == Some(Dialog::AddDevice) && self.add_action(&action) {
            return;
        }
        match self.shown() {
            Page::Adapter(id) => {
                self.adapter_action(&id, &action);
            }
            Page::Device(a, id) => {
                self.device_action(&a, id, &action);
            }
            Page::Overview => {}
        }
    }
    /// Whether the dialog is waiting for a rename, profile operation or device removal.
    pub fn dialog_busy(&self) -> bool {
        match &self.dialog {
            Some(Dialog::Rename(id)) => self.renaming(id),
            Some(Dialog::Forget(adapter, device)) => {
                self.running(adapter, |k| matches!(k, Kind::Forget(id) if id == device))
            }
            Some(Dialog::ProfileName(adapter, _) | Dialog::ProfileDelete(adapter, _)) => {
                self.profiles_busy(adapter)
            }
            _ => false,
        }
    }

    /// Closes the open menu or dialog, unless its request is still running.
    pub(crate) fn cancel(&mut self) {
        if self.menu.take().is_some() {
            return;
        }
        let busy = match self.dialog.clone() {
            Some(Dialog::Rename(id)) => self.renaming(&id),
            Some(Dialog::Forget(a, d)) => {
                self.running(&a, |k| matches!(k, Kind::Forget(x) if *x == d))
            }
            Some(Dialog::ProfileName(a, _) | Dialog::ProfileDelete(a, _)) => {
                self.running(&a, |k| matches!(k, Kind::Profile))
            }
            Some(Dialog::AddDevice) => {
                self.close_add();
                return;
            }
            _ => false,
        };
        if !busy {
            // New Profile opened from the picker returns to it. The picker offers New Profile
            // only while it lists no profiles, so nothing else was chosen.
            self.dialog = match (self.pick_after_new.take(), self.dialog.take()) {
                (Some(purpose), Some(Dialog::ProfileName(adapter, None))) => Some(Dialog::Pick {
                    adapter,
                    purpose,
                    chosen: 0,
                }),
                _ => None,
            };
            self.notes.remove(&Spot::Dialog);
        }
    }
    /// The open dialog's main action.
    pub(crate) fn submit(&mut self) {
        match self.dialog.clone() {
            Some(Dialog::Rename(id)) => self.submit_rename(&id, false),
            Some(Dialog::ProfileName(a, copy)) => self.submit_profile_name(&a, copy),
            Some(Dialog::Pick {
                adapter,
                purpose,
                chosen,
            }) => {
                let none = match purpose {
                    super::PickFor::Interface(i) => self
                        .adapter(&adapter)
                        .and_then(|a| self.staged_interface(a, i))
                        .is_some_and(|s| !s.enabled),
                    super::PickFor::Layer(_) => false,
                };
                if none || chosen != 0 {
                    self.choose_profile(&adapter, purpose, chosen);
                }
            }
            Some(Dialog::Help) => self.dialog = None,
            _ => {}
        }
    }
    /// The open confirmation's answer.
    pub(crate) fn confirm(&mut self) {
        let Some(dialog) = self.dialog.clone() else {
            return;
        };
        match dialog {
            Dialog::Forget(a, id) => self.forget_confirm(&a, id),
            Dialog::Bootloader(a) => {
                self.dialog = None;
                self.run(&a, Kind::Bootloader, Command::Bootloader);
            }
            _ => {}
        }
    }
}
