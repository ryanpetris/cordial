//! CDC management before the application or Bluetooth backend exists.
use super::{Startup, committed_format, startup_error};
use crate::{
    application::Build,
    control::{OutputToken, Session},
    files,
    storage::RecordStore,
};
use alloc::string::String;
use cordial_protocol::{self as p, request::Command, response::Result as R};

pub struct Management {
    pub serial: Session,
    status: p::Status,
    listing: Option<(String, usize)>,
    reader: Option<files::Reader>,
    restarting: bool,
    drained: bool,
    development: bool,
    assisted: bool,
}
impl Management {
    pub fn new(build: Build, startup: Startup) -> Self {
        Self {
            serial: Session::new(),
            status: p::Status {
                id: build.adapter_id,
                name: build.default_adapter_name.into(),
                information: Some(p::AdapterInformation {
                    firmware_version: Some(build.version.into()),
                    board_name: Some(build.board.into()),
                    development_build: Some(build.development),
                    target_storage_format: Some(cordial_protocol::storage::CURRENT_FORMAT),
                    ..Default::default()
                }),
                errors: alloc::vec![match startup {
                    Startup::Blocked(_) => startup_error(),
                    _ => p::Error {
                        code: p::ErrorCode::StorageMigrationRequired as i32,
                        reason: p::ErrorReason::Storage as i32,
                        outcome: p::ErrorOutcome::NoEffect as i32,
                        ..Default::default()
                    },
                }],
                ..Default::default()
            },
            listing: None,
            reader: None,
            restarting: false,
            drained: false,
            development: build.development,
            assisted: startup == Startup::Assisted,
        }
    }
    pub fn session(&mut self, active: bool) {
        self.serial.session(active);
        self.listing = None;
        self.reader = None;
        self.restarting = false;
        self.drained = false;
    }
    pub fn restarting(&self) -> bool {
        self.restarting
    }
    /// Only a successful final USB completion, never a timeout or disconnect, permits reset.
    pub fn reset_ready(&self) -> bool {
        self.drained
    }
    pub fn output_complete(&mut self, token: OutputToken, length: usize) {
        self.serial.output_complete(token, length);
        if self.restarting && self.serial.active() && self.serial.queued() == 0 {
            self.drained = true;
        }
    }
    fn reply(&mut self, result: Option<R>) {
        self.serial.respond(p::Response { result });
    }
    fn fail(&mut self, error: p::Error) {
        self.serial.respond(p::Response {
            result: Some(R::Error(error)),
        });
    }
    pub async fn begin<S: RecordStore>(&mut self, request: p::Request, store: &mut S) {
        if self.restarting {
            return;
        }
        let Some(command) = request.command else {
            return self.fail(p::Error {
                code: p::ErrorCode::UnknownCommand as i32,
                ..Default::default()
            });
        };
        if matches!(command, Command::GetStatus(_)) {
            let mut status = self.status.clone();
            match committed_format(store).await {
                Ok(format) => status.information.as_mut().unwrap().storage_format = Some(format),
                Err(_) => {
                    let error = startup_error();
                    if !status.errors.contains(&error) {
                        status.errors.push(error);
                        status.errors.sort_unstable();
                    }
                }
            }
            return self.reply(Some(R::Status(status)));
        }
        if matches!(command, Command::Restart(_)) {
            self.restarting = true;
            return self.reply(None);
        }
        if !self.assisted {
            return self.fail(self.status.errors[0].clone());
        }
        let result = match command {
            Command::ListFiles(args) => {
                if !crate::model::storage_path(&args.path) {
                    return self.fail(files::failure(crate::storage::Error::Bounds, false));
                }
                self.listing = Some((args.path, 0));
                self.serial.start_stream();
                return;
            }
            Command::ReadFile(args) if args.length.is_none() && self.development => {
                match files::Reader::new(store, args).await {
                    Ok(reader) => {
                        self.reader = Some(reader);
                        self.serial.start_stream();
                    }
                    Err(error) => self.fail(files::failure(error, false)),
                }
                return;
            }
            Command::ReadFile(args) => {
                return match files::read(store, args).await {
                    Ok(data) => self.reply(Some(R::File(data))),
                    Err(error) => self.fail(files::failure(error, false)),
                };
            }
            Command::WriteFile(args) => files::write(store, args).await,
            Command::MoveFile(args) => files::move_file(store, args).await,
            Command::DeleteFile(args) => files::delete(store, args).await,
            _ => return self.fail(self.status.errors[0].clone()),
        };
        match result {
            Ok(()) => self.reply(None),
            Err(error) => self.fail(files::failure(error, true)),
        }
    }
    /// Streams one raw directory entry per frame without retaining the inventory.
    pub async fn proceed<S: RecordStore>(&mut self, store: &mut S) {
        if !self.serial.stream_ready() {
            return;
        }
        if let Some(mut reader) = self.reader.take() {
            if !reader.step(&mut self.serial, store).await {
                self.reader = Some(reader);
            }
            return;
        }
        let Some((path, index)) = self.listing.take() else {
            return;
        };
        match store.file_entry(&path, index).await {
            Ok(entry) => {
                let finished = entry.is_none();
                let entries = entry
                    .into_iter()
                    .map(|entry| p::FileEntry {
                        name: entry.name,
                        directory: entry.kind == crate::storage::FileType::Directory,
                        size: entry.size as u64,
                    })
                    .collect();
                self.serial.stream_response(
                    p::Response {
                        result: Some(R::Files(p::FileList { entries })),
                    },
                    finished,
                );
                if !finished {
                    self.listing = Some((path, index + 1));
                }
            }
            Err(error) => self.fail(files::failure(error, false)),
        }
    }
}
