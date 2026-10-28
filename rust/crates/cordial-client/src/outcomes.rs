//! Host interpretation of completed operations, before responses reach callers or observers.
use crate::{Error, Event, Result};
use cordial_protocol::{self as p, request::Command, response};

fn completed(error: &mut p::Error, log: &dyn Fn(&p::Error)) -> bool {
    if cfg!(debug_assertions) {
        return false;
    }
    if error.outcome == p::ErrorOutcome::Complete as i32 {
        log(error);
        return true;
    }
    error.related.retain_mut(|error| !completed(error, log));
    false
}

fn errors(errors: &mut Vec<p::Error>, log: &dyn Fn(&p::Error)) {
    errors.retain_mut(|error| !completed(error, log));
}

fn device(device: &mut p::Device, log: &dyn Fn(&p::Error)) {
    errors(&mut device.errors, log);
    for integration in &mut device.integrations {
        if let Some(p::integration::Status::Error(error)) = &mut integration.status
            && completed(error, log)
        {
            integration.status = None;
        }
    }
    for output in &mut device.outputs {
        if let Some(p::output_status::Status::Error(error)) = &mut output.status
            && completed(error, log)
        {
            output.status = None;
        }
    }
}

fn settings(objects: &mut [p::SettingsObject], log: &dyn Fn(&p::Error)) {
    fn status(status: &mut Option<p::SettingStatus>, log: &dyn Fn(&p::Error)) {
        if let Some(status) = status
            && let Some(p::setting_status::Result::Error(error)) = &mut status.result
            && completed(error, log)
        {
            status.result = Some(p::setting_status::Result::State(
                p::SettingState::Applied as i32,
            ));
        }
    }
    macro_rules! fields {
        ($object:ident, $($field:ident),+ $(,)?) => {{
            $(if let Some(field) = &mut $object.$field { status(&mut field.status, log); })+
        }};
    }
    use p::settings_object::Group;
    for object in objects {
        match &mut object.group {
            Some(Group::Keyboard(o)) => fields!(o, platform, function_row),
            Some(Group::Power(o)) => fields!(o, auto_off),
            Some(Group::Backlight(o)) => {
                fields!(
                    o,
                    enabled,
                    mode,
                    brightness,
                    startup_effect,
                    dial_effect,
                    power_save,
                    effect
                );
                if let Some(o) = &mut o.delay {
                    fields!(o, hands_away, hands_nearby, external_power);
                }
            }
            Some(Group::PointerSensor(o)) => fields!(o, dpi),
            Some(Group::Wheel(o)) => fields!(o, mode, free_spin_threshold, invert),
            Some(Group::Thumbwheel(o)) => fields!(o, invert),
            None => {}
        }
    }
}

fn record(error: &mut p::RecordError, log: &dyn Fn(&p::Error)) -> Result<()> {
    if error.error.as_mut().is_some_and(|e| completed(e, log)) {
        // Completion cannot supply the record that this variant omits.
        return Err(Error::UnexpectedResponse);
    }
    Ok(())
}

pub(crate) fn response(
    request: &p::Request,
    response: &mut p::Response,
    log: &dyn Fn(&p::Error),
) -> Result<()> {
    use response::Result as R;
    match &mut response.result {
        Some(R::Error(error)) => {
            if !completed(error, log) {
                return Ok(());
            }
            response.result = None;
            match &request.command {
                Some(
                    Command::SetAdapter(_)
                    | Command::Restart(_)
                    | Command::StartScan(_)
                    | Command::StopScan(_)
                    | Command::StartPairing(_)
                    | Command::AcceptPrompt(_)
                    | Command::RejectPrompt(_)
                    | Command::CancelPairing(_)
                    | Command::SetDevice(_)
                    | Command::UnpairDevice(_)
                    | Command::RefreshDevice(_)
                    | Command::SetSettings(_)
                    | Command::DeleteProfile(_)
                    | Command::SetProfileRules(_)
                    | Command::WriteFile(_)
                    | Command::MoveFile(_)
                    | Command::DeleteFile(_)
                    | Command::EnterBootloader(_),
                ) => {}
                _ => return Err(Error::UnexpectedResponse),
            }
        }
        Some(R::Status(status)) => errors(&mut status.errors, log),
        Some(R::Device(d)) => device(d, log),
        Some(R::Devices(list)) => {
            for entry in &mut list.entries {
                match &mut entry.entry {
                    Some(p::device_list_entry::Entry::Device(d)) => device(d, log),
                    Some(p::device_list_entry::Entry::Error(e)) => record(e, log)?,
                    None => {}
                }
            }
        }
        Some(R::Profiles(list)) => {
            for entry in &mut list.entries {
                if let Some(p::profile_list_entry::Entry::Error(e)) = &mut entry.entry {
                    record(e, log)?;
                }
            }
        }
        Some(R::Settings(s)) => settings(&mut s.objects, log),
        _ => {}
    }
    Ok(())
}

pub(crate) fn event(mut event: p::Event, log: &dyn Fn(&p::Error)) -> Event {
    match &mut event.kind {
        Some(p::event::Kind::Adapter(status)) => errors(&mut status.errors, log),
        Some(p::event::Kind::Device(d)) => device(d, log),
        Some(p::event::Kind::SettingsChanged(s)) => settings(&mut s.changed, log),
        Some(p::event::Kind::ScanDone(scan)) => {
            if scan.error.as_mut().is_some_and(|e| completed(e, log)) {
                scan.error = None;
            }
        }
        Some(p::event::Kind::Pairing(pairing)) => {
            if let Some(p::pairing::Step::Error(error)) = &mut pairing.step
                && completed(error, log)
            {
                return Event::PairingComplete {
                    candidate: pairing.candidate,
                };
            }
        }
        _ => {}
    }
    Event::Protocol(event)
}
