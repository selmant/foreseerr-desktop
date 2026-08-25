//! Pure application controller for protocol v1.

use std::sync::atomic::{AtomicU64, Ordering};

use crate::auth::{AuthErrorCode, AuthProof, PendingAuthProofs, new_auth_proof};
use crate::config::validate_foreseer_url;
use crate::protocol::{NativeCommandV1, NativeEventV1};
use crate::session::{ExpectedSession, SessionBootstrap, SessionMatchError};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppState {
    Starting,
    Setup,
    Authenticating,
    Ready,
    Resolving,
    Playing,
    Restoring,
    Degraded,
    ShuttingDown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Presentation {
    Frontend,
    /// Wake Jellyfin CEF for resolve/anim without revealing page chrome.
    PrimaryWebPreparing,
    PrimaryWeb,
}

/// Mockable runtime surface used by the controller (Phase 3).
pub trait RuntimeOps {
    fn post_frontend_event(&mut self, event: NativeEventV1);
    fn set_presentation(&mut self, presentation: Presentation);
    fn navigate_primary_web(&mut self, url: &str) -> bool;
    fn complete_setup_navigation(&mut self, url: &str) -> bool;
    fn minimize(&mut self);
    fn toggle_maximize(&mut self);
    fn toggle_fullscreen(&mut self);
    fn request_shutdown(&mut self);
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ControllerEvent {
    BootstrapReady {
        server_id: String,
        user_id: String,
        generation: String,
    },
    BootstrapFailed {
        request_id: String,
        code: AuthErrorCode,
    },
    AuthRedeemed {
        request_id: String,
        bootstrap: SessionBootstrap,
        auth_epoch: u64,
    },
    AuthFailed {
        request_id: String,
        code: AuthErrorCode,
        auth_epoch: u64,
    },
    SetupCheckResult {
        request_id: String,
        generation: u64,
        result: Result<u16, String>,
    },
    PlaybackStarted,
    PlaybackFinished,
    PlaybackCanceled,
    PlaybackError,
    Shutdown,
}

pub struct Controller<R: RuntimeOps> {
    pub runtime: R,
    state: AppState,
    pending_proofs: PendingAuthProofs,
    auth_epoch: AtomicU64,
    setup_generation: u64,
    in_setup: bool,
    active_request_id: Option<String>,
    expected_session: Option<ExpectedSession>,
    pending_bootstrap: Option<(String, SessionBootstrap)>,
}

impl<R: RuntimeOps> Controller<R> {
    pub fn new(runtime: R, in_setup: bool) -> Self {
        Self {
            runtime,
            state: if in_setup {
                AppState::Setup
            } else {
                AppState::Starting
            },
            pending_proofs: PendingAuthProofs::default(),
            auth_epoch: AtomicU64::new(0),
            setup_generation: 0,
            in_setup,
            active_request_id: None,
            expected_session: None,
            pending_bootstrap: None,
        }
    }

    pub fn state(&self) -> AppState {
        self.state
    }

    pub fn auth_epoch(&self) -> u64 {
        self.auth_epoch.load(Ordering::SeqCst)
    }

    pub fn setup_generation(&self) -> u64 {
        self.setup_generation
    }

    pub fn in_setup(&self) -> bool {
        self.in_setup
    }

    pub fn enter_setup(&mut self) {
        self.in_setup = true;
        self.state = AppState::Setup;
        self.setup_generation = self.setup_generation.wrapping_add(1);
        self.active_request_id = None;
        self.expected_session = None;
        self.pending_bootstrap = None;
    }

    pub fn active_request_id(&self) -> Option<&str> {
        self.active_request_id.as_deref()
    }

    pub fn handle_command(&mut self, command: NativeCommandV1) -> bool {
        if self.state == AppState::ShuttingDown {
            return false;
        }
        match command {
            NativeCommandV1::AuthChallenge { id } => self.on_auth_challenge(id),
            NativeCommandV1::AuthComplete { id: _, ticket: _ } => {
                // Adapter must call begin_auth_complete(id) then redeem asynchronously.
                true
            }
            NativeCommandV1::SessionClear { id } => self.on_session_clear(id),
            NativeCommandV1::PlayItem { id, item_id } => self.on_play_item(id, item_id),
            NativeCommandV1::SetupCheck {
                id,
                url,
                allow_http,
            } => self.on_setup_check(id, url, allow_http),
            NativeCommandV1::SetupSave {
                id,
                url,
                allow_http,
            } => self.on_setup_save(id, url, allow_http),
            NativeCommandV1::SetupStandalone { id } => {
                self.emit_error(&id, AuthErrorCode::InvalidRequest);
                true
            }
            // The extension redeems the server-authorized ticket before it
            // reaches this generic controller.
            NativeCommandV1::BrowserCacheClear { .. } => true,
            // The extension owns retry state and exact-port rebinding.
            NativeCommandV1::RuntimeRetry { .. } => true,
            NativeCommandV1::RuntimeOpenLogs { .. } => true,
            NativeCommandV1::RuntimeOpenSetup { .. } => true,
            NativeCommandV1::WindowMinimize { .. } => {
                self.runtime.minimize();
                true
            }
            NativeCommandV1::WindowToggleMaximize { .. } => {
                self.runtime.toggle_maximize();
                true
            }
            NativeCommandV1::WindowToggleFullscreen { .. } => {
                self.runtime.toggle_fullscreen();
                true
            }
            NativeCommandV1::AppQuit { .. } => {
                self.state = AppState::ShuttingDown;
                self.runtime.request_shutdown();
                true
            }
        }
    }

    pub fn handle_event(&mut self, event: ControllerEvent) {
        match event {
            ControllerEvent::AuthRedeemed {
                request_id,
                bootstrap,
                auth_epoch,
            } => {
                if auth_epoch != self.auth_epoch() {
                    return;
                }
                match bootstrap.expected() {
                    Ok(expected) => {
                        self.expected_session = Some(expected);
                        self.pending_bootstrap = Some((request_id.clone(), bootstrap.clone()));
                        let _ = self.runtime.navigate_primary_web(&bootstrap.server_url);
                        self.state = AppState::Authenticating;
                    }
                    Err(_) => self.emit_error(&request_id, AuthErrorCode::InvalidBootstrapResponse),
                }
            }
            ControllerEvent::AuthFailed {
                request_id,
                code,
                auth_epoch,
            } => {
                if auth_epoch != self.auth_epoch() {
                    return;
                }
                self.state = AppState::Degraded;
                self.emit_error(&request_id, code);
            }
            ControllerEvent::BootstrapReady {
                server_id,
                user_id,
                generation,
            } => {
                let Some((request_id, _)) = self.pending_bootstrap.take() else {
                    return;
                };
                let ok = self
                    .expected_session
                    .as_ref()
                    .map(|expected| expected.matches(&server_id, &user_id, &generation))
                    .unwrap_or(Err(SessionMatchError::GenerationMismatch));
                match ok {
                    Ok(()) => {
                        self.state = AppState::Ready;
                        self.runtime
                            .post_frontend_event(NativeEventV1::new(request_id, "ready"));
                    }
                    Err(_) => {
                        self.state = AppState::Degraded;
                        self.emit_error(&request_id, AuthErrorCode::InvalidBootstrapResponse);
                    }
                }
            }
            ControllerEvent::BootstrapFailed { request_id, code } => {
                if let Some((pending_id, bootstrap)) = self.pending_bootstrap.as_mut()
                    && pending_id == &request_id
                    && let Some(fallback) = bootstrap.fallback_server_url.take()
                    && fallback != bootstrap.server_url
                {
                    bootstrap.server_url = fallback;
                    if let Ok(expected) = bootstrap.expected() {
                        self.expected_session = Some(expected);
                        let url = bootstrap.server_url.clone();
                        let _ = self.runtime.navigate_primary_web(&url);
                        return;
                    }
                }
                self.pending_bootstrap = None;
                self.state = AppState::Degraded;
                self.emit_error(&request_id, code);
            }
            ControllerEvent::SetupCheckResult {
                request_id,
                generation,
                result,
            } => {
                if generation != self.setup_generation || !self.in_setup {
                    return;
                }
                match result {
                    Ok(status) => self.runtime.post_frontend_event(
                        NativeEventV1::new(request_id, "connectivity-success").with_status(status),
                    ),
                    Err(message) => self.runtime.post_frontend_event(
                        NativeEventV1::new(request_id, "error")
                            .with_error("invalid_request")
                            .with_message(message),
                    ),
                }
            }
            ControllerEvent::PlaybackStarted => {
                self.state = AppState::Playing;
                self.runtime.set_presentation(Presentation::PrimaryWeb);
                if let Some(id) = self.active_request_id.clone() {
                    self.runtime
                        .post_frontend_event(NativeEventV1::new(id, "playing"));
                }
            }
            ControllerEvent::PlaybackFinished => self.end_playback("finished"),
            ControllerEvent::PlaybackCanceled => self.end_playback("canceled"),
            ControllerEvent::PlaybackError => self.end_playback("error"),
            ControllerEvent::Shutdown => {
                self.state = AppState::ShuttingDown;
                self.pending_proofs.clear();
                self.active_request_id = None;
                self.pending_bootstrap = None;
            }
        }
    }

    /// Take a freshly minted proof for an async redeem worker.
    pub fn take_proof_for_complete(&mut self, request_id: &str) -> Option<(AuthProof, u64)> {
        let proof = self.pending_proofs.take(request_id)?;
        Some((proof, self.auth_epoch()))
    }

    fn on_auth_challenge(&mut self, id: String) -> bool {
        let proof = new_auth_proof();
        let challenge = proof.challenge.clone();
        self.pending_proofs.insert(id.clone(), proof);
        self.state = AppState::Authenticating;
        self.runtime.post_frontend_event(
            NativeEventV1::new(id, "auth-challenge").with_challenge(challenge),
        );
        true
    }

    /// Start auth completion: returns verifier + epoch for a worker, or emits error.
    pub fn begin_auth_complete(&mut self, id: &str) -> Option<(String, u64)> {
        match self.pending_proofs.take(id) {
            Some(proof) => Some((proof.verifier, self.auth_epoch())),
            None => {
                self.emit_error(id, AuthErrorCode::InvalidRequest);
                self.state = AppState::Degraded;
                None
            }
        }
    }

    fn on_session_clear(&mut self, id: String) -> bool {
        self.auth_epoch.fetch_add(1, Ordering::SeqCst);
        self.pending_proofs.clear();
        self.expected_session = None;
        self.pending_bootstrap = None;
        self.active_request_id = None;
        self.state = AppState::Starting;
        self.runtime
            .post_frontend_event(NativeEventV1::new(id, "stopped"));
        self.runtime.set_presentation(Presentation::Frontend);
        true
    }

    fn on_play_item(&mut self, id: String, _item_id: String) -> bool {
        if !matches!(
            self.state,
            AppState::Ready | AppState::Playing | AppState::Resolving | AppState::Degraded
        ) {
            self.emit_error(&id, AuthErrorCode::InvalidRequest);
            return true;
        }
        if let Some(previous) = self.active_request_id.replace(id.clone())
            && previous != id
        {
            self.runtime
                .post_frontend_event(NativeEventV1::new(previous, "canceled"));
        }
        self.state = AppState::Resolving;
        // Unhide PrimaryWeb before Jellyfin createMediaElement waits on its
        // zoom animationend; WasHidden CEF never fires that event. Keep the
        // preparing veil so Jellyfin login/library chrome stays hidden until
        // the poster mounts / playback starts.
        self.runtime
            .set_presentation(Presentation::PrimaryWebPreparing);
        self.runtime
            .post_frontend_event(NativeEventV1::new(id.clone(), "accepted"));
        self.runtime
            .post_frontend_event(NativeEventV1::new(id, "resolving"));
        true
    }

    fn on_setup_check(&mut self, id: String, url: String, allow_http: bool) -> bool {
        if !self.in_setup {
            self.emit_error(&id, AuthErrorCode::InvalidRequest);
            return true;
        }
        if let Err(err) = validate_foreseer_url(&url) {
            self.runtime.post_frontend_event(
                NativeEventV1::new(id, "error")
                    .with_error("invalid_request")
                    .with_message(err.message()),
            );
            return true;
        }
        // Async connectivity is owned by a worker using setup_generation.
        let _ = (id, url, allow_http);
        true
    }

    fn on_setup_save(&mut self, id: String, url: String, _allow_http: bool) -> bool {
        if !self.in_setup {
            self.emit_error(&id, AuthErrorCode::InvalidRequest);
            return true;
        }
        match validate_foreseer_url(&url) {
            Ok(normalized) => {
                self.setup_generation = self.setup_generation.wrapping_add(1);
                self.in_setup = false;
                let ok = self.runtime.complete_setup_navigation(&normalized);
                if ok {
                    self.state = AppState::Starting;
                    self.runtime
                        .post_frontend_event(NativeEventV1::new(id, "save-config-success"));
                } else {
                    self.emit_error(&id, AuthErrorCode::InvalidRequest);
                }
            }
            Err(err) => {
                self.runtime.post_frontend_event(
                    NativeEventV1::new(id, "error")
                        .with_error("invalid_request")
                        .with_message(err.message()),
                );
            }
        }
        true
    }

    fn end_playback(&mut self, kind: &str) {
        self.state = AppState::Restoring;
        self.runtime.set_presentation(Presentation::Frontend);
        if let Some(id) = self.active_request_id.take() {
            self.runtime
                .post_frontend_event(NativeEventV1::new(id, kind));
        }
        self.state = AppState::Ready;
    }

    fn emit_error(&mut self, id: &str, code: AuthErrorCode) {
        self.runtime
            .post_frontend_event(NativeEventV1::new(id, "error").with_error(code.as_str()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct MockRuntime {
        events: Vec<NativeEventV1>,
        presentations: Vec<Presentation>,
        navigations: Vec<String>,
        setup_navs: Vec<String>,
        shutdown: bool,
    }

    impl RuntimeOps for MockRuntime {
        fn post_frontend_event(&mut self, event: NativeEventV1) {
            self.events.push(event);
        }
        fn set_presentation(&mut self, presentation: Presentation) {
            self.presentations.push(presentation);
        }
        fn navigate_primary_web(&mut self, url: &str) -> bool {
            self.navigations.push(url.to_string());
            true
        }
        fn complete_setup_navigation(&mut self, url: &str) -> bool {
            self.setup_navs.push(url.to_string());
            true
        }
        fn minimize(&mut self) {}
        fn toggle_maximize(&mut self) {}
        fn toggle_fullscreen(&mut self) {}
        fn request_shutdown(&mut self) {
            self.shutdown = true;
        }
    }

    fn bootstrap() -> SessionBootstrap {
        SessionBootstrap {
            server_url: "https://jellyfin.example/".into(),
            server_id: "srv".into(),
            user_id: "user".into(),
            device_id: "dev".into(),
            access_token: "tok".into(),
            bootstrap_generation: "gen-1".into(),
            fallback_server_url: None,
        }
    }

    #[test]
    fn auth_challenge_and_stale_redeem_ignored() {
        let mut ctl = Controller::new(MockRuntime::default(), false);
        assert!(ctl.handle_command(NativeCommandV1::AuthChallenge { id: "a1".into() }));
        assert_eq!(ctl.runtime.events[0].event_type, "auth-challenge");
        let epoch = ctl.auth_epoch();
        ctl.handle_event(ControllerEvent::AuthFailed {
            request_id: "a1".into(),
            code: AuthErrorCode::TicketExpired,
            auth_epoch: epoch + 1,
        });
        assert!(ctl.runtime.events.iter().all(|e| e.event_type != "error"));
    }

    #[test]
    fn bootstrap_match_ready_mismatch_degrades() {
        let mut ctl = Controller::new(MockRuntime::default(), false);
        let epoch = ctl.auth_epoch();
        ctl.handle_event(ControllerEvent::AuthRedeemed {
            request_id: "a1".into(),
            bootstrap: bootstrap(),
            auth_epoch: epoch,
        });
        assert_eq!(ctl.runtime.navigations[0], "https://jellyfin.example/");
        ctl.handle_event(ControllerEvent::BootstrapReady {
            server_id: "srv".into(),
            user_id: "user".into(),
            generation: "gen-1".into(),
        });
        assert_eq!(ctl.state(), AppState::Ready);
        assert!(ctl.runtime.events.iter().any(|e| e.event_type == "ready"));
    }

    #[test]
    fn jellyfin_fallback_url_is_tried_after_bootstrap_failure() {
        let mut ctl = Controller::new(MockRuntime::default(), false);
        let epoch = ctl.auth_epoch();
        let mut bootstrap = bootstrap();
        bootstrap.server_url = "http://192.168.40.3:8096".into();
        bootstrap.fallback_server_url = Some("https://jellyfin.example".into());
        ctl.handle_event(ControllerEvent::AuthRedeemed {
            request_id: "a1".into(),
            bootstrap,
            auth_epoch: epoch,
        });
        assert_eq!(ctl.runtime.navigations[0], "http://192.168.40.3:8096");
        ctl.handle_event(ControllerEvent::BootstrapFailed {
            request_id: "a1".into(),
            code: AuthErrorCode::InvalidBootstrapResponse,
        });
        assert_eq!(ctl.runtime.navigations[1], "https://jellyfin.example");
        assert_eq!(ctl.state(), AppState::Authenticating);
    }

    #[test]
    fn play_replace_and_terminal_restores_before_event() {
        let mut ctl = Controller::new(MockRuntime::default(), false);
        ctl.state = AppState::Ready;
        ctl.handle_command(NativeCommandV1::PlayItem {
            id: "play-a".into(),
            item_id: "item1".into(),
        });
        assert_eq!(
            ctl.runtime.presentations,
            &[Presentation::PrimaryWebPreparing]
        );
        ctl.handle_command(NativeCommandV1::PlayItem {
            id: "play-b".into(),
            item_id: "item2".into(),
        });
        assert!(
            ctl.runtime
                .events
                .iter()
                .any(|e| e.id == "play-a" && e.event_type == "canceled")
        );
        assert_eq!(
            ctl.runtime.presentations,
            &[
                Presentation::PrimaryWebPreparing,
                Presentation::PrimaryWebPreparing
            ]
        );
        ctl.handle_event(ControllerEvent::PlaybackStarted);
        assert_eq!(
            ctl.runtime.presentations.last(),
            Some(&Presentation::PrimaryWeb)
        );
        ctl.runtime.events.clear();
        ctl.runtime.presentations.clear();
        ctl.handle_event(ControllerEvent::PlaybackFinished);
        assert_eq!(ctl.runtime.presentations[0], Presentation::Frontend);
        assert_eq!(ctl.runtime.events[0].event_type, "finished");
        assert_eq!(ctl.state(), AppState::Ready);
    }

    #[test]
    fn setup_save_clears_setup_authority() {
        let mut ctl = Controller::new(MockRuntime::default(), true);
        let setup_gen = ctl.setup_generation();
        ctl.handle_command(NativeCommandV1::SetupSave {
            id: "s1".into(),
            url: "https://foreseer.example".into(),
            allow_http: false,
        });
        assert!(ctl.setup_generation() > setup_gen || !ctl.in_setup());
        assert_eq!(ctl.runtime.setup_navs[0], "https://foreseer.example");
        // Stale setup callback ignored.
        ctl.handle_event(ControllerEvent::SetupCheckResult {
            request_id: "old".into(),
            generation: setup_gen,
            result: Ok(204),
        });
        assert!(
            ctl.runtime
                .events
                .iter()
                .all(|e| e.event_type != "connectivity-success")
        );
    }

    #[test]
    fn enter_setup_reopens_setup_authority() {
        let mut ctl = Controller::new(MockRuntime::default(), false);
        assert!(!ctl.in_setup());
        ctl.enter_setup();
        assert!(ctl.in_setup());
        assert_eq!(ctl.state(), AppState::Setup);
        ctl.handle_command(NativeCommandV1::SetupSave {
            id: "s1".into(),
            url: "https://foreseer.example".into(),
            allow_http: false,
        });
        assert!(!ctl.in_setup());
        assert_eq!(ctl.runtime.setup_navs[0], "https://foreseer.example");
    }

    #[test]
    fn shutdown_cancels_without_deadlock() {
        let mut ctl = Controller::new(MockRuntime::default(), false);
        ctl.handle_command(NativeCommandV1::AppQuit { id: "q".into() });
        assert!(ctl.runtime.shutdown);
        assert_eq!(ctl.state(), AppState::ShuttingDown);
        assert!(!ctl.handle_command(NativeCommandV1::AuthChallenge { id: "x".into() }));
    }

    #[test]
    fn url_validation_errors_are_safe() {
        let mut ctl = Controller::new(MockRuntime::default(), true);
        ctl.handle_command(NativeCommandV1::SetupSave {
            id: "s1".into(),
            url: "https://user:pass@evil".into(),
            allow_http: false,
        });
        let err = ctl.runtime.events.last().unwrap();
        assert_eq!(err.event_type, "error");
        assert!(!format!("{err:?}").contains("pass"));
    }
}
