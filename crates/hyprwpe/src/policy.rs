//! Policy engine: manages renderer lifecycle states (Active, Suspended)
//! driven by Hyprland visibility, occlusion, display power, and hysteresis.
//!
//! A wallpaper nobody can see should burn zero CPU. Occlusion is common and brief
//! (e.g. quick workspace switching, alt-tabbing), so transitions to `Suspended`
//! are delayed by a hysteretic threshold (1.5s) to avoid renderer thrashing.
//! Returning to a visible desktop resumes immediately.

use hyprwpe_core::hyprland::{HyprEvent, OcclusionTracker};
use std::time::{Duration, Instant};

/// Lifecycle state of wallpaper renderers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PolicyState {
    /// Renderers active and advancing frames.
    Active,
    /// Renderers suspended (paused, 0% CPU, surface mapped).
    Suspended,
}

/// Action to apply to the renderers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PolicyAction {
    None,
    Pause,
    Resume,
}

/// The policy engine tracking visibility and driving hysteretic state changes.
pub struct PolicyEngine {
    state: PolicyState,
    tracker: OcclusionTracker,
    manual_paused: bool,
    pending_suspend_deadline: Option<Instant>,
    suspend_delay: Duration,
}

impl Default for PolicyEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl PolicyEngine {
    /// Create a new policy engine with the default 1.5s hysteresis delay.
    pub fn new() -> Self {
        Self::with_delay(Duration::from_millis(1500))
    }

    /// Create a policy engine with a custom hysteresis delay.
    pub fn with_delay(suspend_delay: Duration) -> Self {
        Self {
            state: PolicyState::Active,
            tracker: OcclusionTracker::new(),
            manual_paused: false,
            pending_suspend_deadline: None,
            suspend_delay,
        }
    }

    /// Set manual pause mode (from CLI `hyprwpe pause` / `resume`).
    ///
    /// When manual pause is active, renderers stay suspended regardless of
    /// compositor events until manually resumed.
    pub fn set_manual_pause(&mut self, paused: bool) -> PolicyAction {
        self.manual_paused = paused;
        self.pending_suspend_deadline = None;
        if paused {
            if self.state != PolicyState::Suspended {
                self.state = PolicyState::Suspended;
                PolicyAction::Pause
            } else {
                PolicyAction::None
            }
        } else if self.tracker.is_all_occluded() {
            // Unpausing while still occluded: remain suspended automatically.
            PolicyAction::None
        } else {
            self.state = PolicyState::Active;
            PolicyAction::Resume
        }
    }

    /// Synchronize initial monitor and window state from Hyprland request socket.
    pub fn sync_from_hyprland(&mut self) {
        self.tracker.sync_from_hyprland();
        self.reconcile(Instant::now());
    }

    /// Handle a Hyprland event and return any action to take immediately.
    pub fn handle_event(&mut self, event: &HyprEvent) -> PolicyAction {
        self.handle_event_at(event, Instant::now())
    }

    /// Handle a Hyprland event at an explicit timestamp (for testing).
    pub fn handle_event_at(&mut self, event: &HyprEvent, now: Instant) -> PolicyAction {
        self.tracker.handle_event(event);
        if self.manual_paused {
            return PolicyAction::None;
        }
        self.reconcile(now)
    }

    /// Reconcile current occlusion state against lifecycle state.
    pub fn reconcile(&mut self, now: Instant) -> PolicyAction {
        let occluded = self.tracker.is_all_occluded();
        if occluded {
            match self.state {
                PolicyState::Active => {
                    // Lockscreen or screens powered down: suspend immediately.
                    if self.tracker.is_locked() || self.tracker.is_dpms_off() {
                        self.pending_suspend_deadline = None;
                        self.state = PolicyState::Suspended;
                        PolicyAction::Pause
                    } else if self.pending_suspend_deadline.is_none() {
                        self.pending_suspend_deadline = Some(now + self.suspend_delay);
                        PolicyAction::None
                    } else {
                        PolicyAction::None
                    }
                }
                PolicyState::Suspended => {
                    self.pending_suspend_deadline = None;
                    PolicyAction::None
                }
            }
        } else {
            // At least one output is visible!
            self.pending_suspend_deadline = None;
            if self.state == PolicyState::Suspended {
                self.state = PolicyState::Active;
                PolicyAction::Resume
            } else {
                PolicyAction::None
            }
        }
    }

    /// Advance the timer and fire any expired hysteretic transitions.
    pub fn tick(&mut self) -> PolicyAction {
        self.tick_at(Instant::now())
    }

    /// Advance with an explicit timestamp (for testing).
    pub fn tick_at(&mut self, now: Instant) -> PolicyAction {
        if self.manual_paused {
            return PolicyAction::None;
        }
        if let Some(deadline) = self.pending_suspend_deadline {
            if now >= deadline {
                self.pending_suspend_deadline = None;
                if self.tracker.is_all_occluded() && self.state == PolicyState::Active {
                    self.state = PolicyState::Suspended;
                    return PolicyAction::Pause;
                }
            }
        }
        PolicyAction::None
    }

    /// Duration until the next hysteretic transition, if one is pending.
    pub fn pending_timeout(&self) -> Option<Duration> {
        self.pending_timeout_at(Instant::now())
    }

    pub fn pending_timeout_at(&self, now: Instant) -> Option<Duration> {
        self.pending_suspend_deadline.map(|deadline| {
            if now >= deadline {
                Duration::ZERO
            } else {
                deadline - now
            }
        })
    }

    /// Whether rendering is currently suspended.
    pub fn is_paused(&self) -> bool {
        self.state == PolicyState::Suspended
    }

    /// Whether manual pause mode is active.
    #[allow(dead_code)]
    pub fn is_manual_paused(&self) -> bool {
        self.manual_paused
    }

    /// Current lifecycle state.
    #[allow(dead_code)]
    pub fn state(&self) -> PolicyState {
        self.state
    }

    /// Occlusion tracker access.
    #[allow(dead_code)]
    pub fn tracker(&self) -> &OcclusionTracker {
        &self.tracker
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hysteresis_prevents_thrashing_on_quick_switch() {
        let now = Instant::now();
        let mut policy = PolicyEngine::with_delay(Duration::from_millis(1500));
        policy.handle_event_at(
            &HyprEvent::FocusedMon {
                name: "DP-2".into(),
                workspace: "1".into(),
            },
            now,
        );

        // Window becomes fullscreen on workspace 1
        assert_eq!(
            policy.handle_event_at(&HyprEvent::Fullscreen(true), now),
            PolicyAction::None
        );
        assert_eq!(policy.state(), PolicyState::Active);
        assert!(policy.pending_timeout_at(now).is_some());

        // Switch to workspace 2 quickly at now + 300ms
        let t_switch = now + Duration::from_millis(300);
        assert_eq!(
            policy.handle_event_at(&HyprEvent::Workspace("2".into()), t_switch),
            PolicyAction::None
        );
        assert_eq!(policy.state(), PolicyState::Active);
        assert_eq!(policy.pending_timeout_at(t_switch), None);

        // Tick past original 1500ms delay -> still Active, no pause occurred!
        assert_eq!(
            policy.tick_at(now + Duration::from_millis(1600)),
            PolicyAction::None
        );
        assert_eq!(policy.state(), PolicyState::Active);
    }

    #[test]
    fn continuous_occlusion_triggers_suspend_and_instant_resume() {
        let now = Instant::now();
        let mut policy = PolicyEngine::with_delay(Duration::from_millis(1500));
        policy.handle_event_at(
            &HyprEvent::FocusedMon {
                name: "DP-2".into(),
                workspace: "1".into(),
            },
            now,
        );

        // Fullscreen at now
        assert_eq!(
            policy.handle_event_at(&HyprEvent::Fullscreen(true), now),
            PolicyAction::None
        );

        // Advance time past threshold (1500ms)
        let action = policy.tick_at(now + Duration::from_millis(1501));
        assert_eq!(action, PolicyAction::Pause);
        assert_eq!(policy.state(), PolicyState::Suspended);

        // Leaving fullscreen resumes instantly (zero delay)
        let action =
            policy.handle_event_at(&HyprEvent::Fullscreen(false), now + Duration::from_millis(2000));
        assert_eq!(action, PolicyAction::Resume);
        assert_eq!(policy.state(), PolicyState::Active);
    }

    #[test]
    fn lock_and_dpms_suspend_immediately() {
        let mut policy = PolicyEngine::with_delay(Duration::from_millis(1500));
        policy.handle_event(&HyprEvent::FocusedMon {
            name: "DP-2".into(),
            workspace: "1".into(),
        });

        // Lockscreen suspends immediately without hysteresis delay
        let action = policy.handle_event(&HyprEvent::Lock(true));
        assert_eq!(action, PolicyAction::Pause);
        assert_eq!(policy.state(), PolicyState::Suspended);

        // Unlock resumes immediately
        let action = policy.handle_event(&HyprEvent::Lock(false));
        assert_eq!(action, PolicyAction::Resume);
        assert_eq!(policy.state(), PolicyState::Active);

        // DPMS display off suspends immediately
        let action = policy.handle_event(&HyprEvent::Dpms(false));
        assert_eq!(action, PolicyAction::Pause);
        assert_eq!(policy.state(), PolicyState::Suspended);

        // DPMS display on resumes immediately
        let action = policy.handle_event(&HyprEvent::Dpms(true));
        assert_eq!(action, PolicyAction::Resume);
        assert_eq!(policy.state(), PolicyState::Active);
    }

    #[test]
    fn manual_pause_overrides_compositor_events() {
        let mut policy = PolicyEngine::new();
        policy.handle_event(&HyprEvent::FocusedMon {
            name: "DP-2".into(),
            workspace: "1".into(),
        });

        assert_eq!(policy.set_manual_pause(true), PolicyAction::Pause);
        assert_eq!(policy.state(), PolicyState::Suspended);
        assert!(policy.is_manual_paused());

        // Event arriving while manually paused must NOT resume
        assert_eq!(
            policy.handle_event(&HyprEvent::Workspace("2".into())),
            PolicyAction::None
        );
        assert_eq!(policy.state(), PolicyState::Suspended);

        // Manual resume restores Active
        assert_eq!(policy.set_manual_pause(false), PolicyAction::Resume);
        assert_eq!(policy.state(), PolicyState::Active);
        assert!(!policy.is_manual_paused());
    }
}
