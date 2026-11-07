// Modified by enzUI contributors. See FORK.md.
use std::fmt;

/// The native protocol that consumes activation context.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActivationPlatform {
    /// Wayland xdg-activation.
    Wayland,
    /// X11 startup notification.
    X11,
}

/// An opaque, single-use desktop activation token.
///
/// Tokens authorize one compositor-mediated handoff. Formatting is always
/// redacted; use [`Self::into_raw`] only at the native protocol or IPC boundary.
#[derive(PartialEq, Eq)]
pub struct ActivationToken {
    wayland: Option<String>,
    x11: Option<String>,
    native: Option<String>,
}

impl ActivationToken {
    /// Wraps a token received from a trusted native transport.
    #[must_use]
    pub fn from_raw(token: String) -> Self {
        Self {
            wayland: None,
            x11: None,
            native: Some(token),
        }
    }

    /// Wraps context explicitly associated with one window protocol.
    #[must_use]
    pub fn for_platform(platform: ActivationPlatform, token: String) -> Self {
        match platform {
            ActivationPlatform::Wayland => Self {
                wayland: Some(token),
                x11: None,
                native: None,
            },
            ActivationPlatform::X11 => Self {
                wayland: None,
                x11: Some(token),
                native: None,
            },
        }
    }

    /// Retains both fields of desktop platform data, when either is present.
    #[must_use]
    pub fn from_platform_data(wayland: Option<String>, x11: Option<String>) -> Option<Self> {
        (wayland.is_some() || x11.is_some()).then_some(Self {
            wayland,
            x11,
            native: None,
        })
    }

    /// The Wayland field for a trusted IPC handoff.
    #[must_use]
    pub fn xdg_activation_token(&self) -> Option<&str> {
        self.wayland.as_deref()
    }

    /// The X11 field for a trusted IPC handoff.
    #[must_use]
    pub fn desktop_startup_id(&self) -> Option<&str> {
        self.x11.as_deref()
    }

    /// A native token whose signal did not identify its protocol.
    ///
    /// This value can be submitted to a native window. Cross-process handoffs
    /// must select the intended platform explicitly.
    #[must_use]
    pub fn untyped_token(&self) -> Option<&str> {
        self.native.as_deref()
    }

    /// Consumes context for the selected native protocol, discarding the other field.
    ///
    /// Returns `None` if explicitly typed context belongs only to another protocol.
    #[must_use]
    pub fn into_raw_for(self, platform: ActivationPlatform) -> Option<String> {
        self.native.or(match platform {
            ActivationPlatform::Wayland => self.wayland,
            ActivationPlatform::X11 => self.x11,
        })
    }

    /// Exposes one raw value, preferring the Wayland field in combined platform data.
    ///
    /// Native backends should use [`Self::into_raw_for`] to select their own field.
    #[must_use]
    pub fn into_raw(self) -> String {
        self.native
            .or(self.wayland)
            .or(self.x11)
            .unwrap_or_default()
    }
}

impl fmt::Debug for ActivationToken {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ActivationToken([redacted])")
    }
}

/// Why an activation-token operation could not be started.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActivationError {
    /// The requested window no longer exists.
    WindowClosed,
    /// The native backend does not support the activation operation or context protocol.
    Unsupported,
    /// The bounded native request queue is full.
    Busy,
    /// The native request did not complete before its deadline.
    Expired,
}

#[cfg(test)]
mod tests {
    use super::ActivationToken;

    #[test]
    fn debug_never_exposes_the_token() {
        let marker = "secret-activation-token";
        let shown = format!("{:?}", ActivationToken::from_raw(marker.into()));

        assert_eq!(shown, "ActivationToken([redacted])");
        assert!(!shown.contains(marker));
    }

    #[test]
    fn raw_value_is_available_only_by_consuming_the_token() {
        let token = ActivationToken::from_raw("handoff".into());
        assert_eq!(token.into_raw(), "handoff");
    }
}
