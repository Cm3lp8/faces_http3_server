//! Errors from the blocking network worker, including its startup prerequisites.
use std::{error::Error, fmt, io};

#[derive(Debug)]
#[non_exhaustive]
pub enum ServerRunError {
    Configuration(&'static str),
    Io {
        phase: &'static str,
        source: io::Error,
    },
    Quic {
        phase: &'static str,
        source: quiche::Error,
    },
    Http3(quiche::h3::Error),
    Entropy(&'static str),
    WorkerPanicked,
    WorkerStopped,
}

impl ServerRunError {
    pub(crate) fn io(phase: &'static str, source: io::Error) -> Self {
        Self::Io { phase, source }
    }

    pub(crate) fn quic(phase: &'static str, source: quiche::Error) -> Self {
        Self::Quic { phase, source }
    }

    pub fn phase(&self) -> &'static str {
        match self {
            Self::Configuration(phase)
            | Self::Entropy(phase)
            | Self::Io { phase, .. }
            | Self::Quic { phase, .. } => phase,
            Self::WorkerPanicked | Self::WorkerStopped => "network_worker",
            Self::Http3(_) => "http3_config",
        }
    }

    pub fn category(&self) -> &'static str {
        match self {
            Self::Configuration(_) => "configuration",
            Self::Io { .. } => "io",
            Self::Quic { .. } => "quic",
            Self::Http3(_) => "http3",
            Self::Entropy(_) => "entropy",
            Self::WorkerPanicked => "worker_panic",
            Self::WorkerStopped => "worker_stopped",
        }
    }
}

impl fmt::Display for ServerRunError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "category={} phase={}", self.category(), self.phase())?;
        match self {
            Self::Io { source, .. } => write!(f, " cause={source}"),
            Self::Quic { source, .. } => write!(f, " cause={source}"),
            Self::Http3(source) => write!(f, " cause={source}"),
            _ => Ok(()),
        }
    }
}

impl Error for ServerRunError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::Quic { source, .. } => Some(source),
            Self::Http3(source) => Some(source),
            _ => None,
        }
    }
}
