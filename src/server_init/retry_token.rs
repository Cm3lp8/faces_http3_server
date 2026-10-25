//! Process-local, authenticated QUIC Retry tokens. This is address validation,
//! not single-use replay protection or HTTP early-data replay protection.
use std::{
    net::{IpAddr, SocketAddr},
    time::{Duration, Instant},
};

use ring::{hmac, rand::SystemRandom};

const VERSION: u8 = 1;
const HEADER_LEN: usize = 10;
const TAG_LEN: usize = 32;
const MIN_ODCID_LEN: usize = 8;
const TTL_MILLIS: u64 = 30_000;
const DOMAIN: &[u8] = b"faces-quic-retry-v1\0";

pub(super) struct RetryTokens {
    key: hmac::Key,
    started: Instant,
}

impl RetryTokens {
    pub(super) fn new() -> Result<Self, ring::error::Unspecified> {
        Ok(Self {
            key: hmac::Key::generate(hmac::HMAC_SHA256, &SystemRandom::new())?,
            started: Instant::now(),
        })
    }

    pub(super) fn mint(
        &self,
        odcid: &[u8],
        retry_cid: &[u8],
        source: &SocketAddr,
    ) -> Option<Vec<u8>> {
        self.mint_at(odcid, retry_cid, source, self.started.elapsed())
    }

    pub(super) fn validate<'a>(
        &self,
        token: &'a [u8],
        retry_cid: &[u8],
        source: &SocketAddr,
    ) -> Option<quiche::ConnectionId<'a>> {
        self.validate_at(token, retry_cid, source, self.started.elapsed())
    }

    fn mint_at(
        &self,
        odcid: &[u8],
        retry_cid: &[u8],
        source: &SocketAddr,
        now: Duration,
    ) -> Option<Vec<u8>> {
        // RFC 9000 requires at least eight bytes in the first Initial DCID.
        // This server always emits a MAX_CONN_ID_LEN Retry SCID.
        if !(MIN_ODCID_LEN..=quiche::MAX_CONN_ID_LEN).contains(&odcid.len())
            || retry_cid.len() != quiche::MAX_CONN_ID_LEN
        {
            return None;
        }
        let issued: u64 = now.as_millis().try_into().ok()?;
        let mut token = Vec::with_capacity(HEADER_LEN + odcid.len() + TAG_LEN);
        token.push(VERSION);
        token.extend_from_slice(&issued.to_be_bytes());
        token.push(odcid.len().try_into().ok()?);
        token.extend_from_slice(odcid);
        let input = authentication_input(&token, retry_cid, source);
        token.extend_from_slice(hmac::sign(&self.key, &input).as_ref());
        Some(token)
    }

    fn validate_at<'a>(
        &self,
        token: &'a [u8],
        retry_cid: &[u8],
        source: &SocketAddr,
        now: Duration,
    ) -> Option<quiche::ConnectionId<'a>> {
        // Bound all parsing and authentication work before inspecting the token.
        if !(HEADER_LEN + MIN_ODCID_LEN + TAG_LEN..=HEADER_LEN + quiche::MAX_CONN_ID_LEN + TAG_LEN)
            .contains(&token.len())
            || retry_cid.len() != quiche::MAX_CONN_ID_LEN
        {
            return None;
        }
        let header = token.get(..HEADER_LEN)?;
        let odcid_len = usize::from(*header.get(9)?);
        if *header.first()? != VERSION
            || !(MIN_ODCID_LEN..=quiche::MAX_CONN_ID_LEN).contains(&odcid_len)
            || token.len() != HEADER_LEN + odcid_len + TAG_LEN
        {
            return None;
        }
        let body_len = HEADER_LEN + odcid_len;
        let input = authentication_input(token.get(..body_len)?, retry_cid, source);
        hmac::verify(&self.key, &input, token.get(body_len..)?).ok()?;
        let issued = u64::from_be_bytes(header.get(1..9)?.try_into().ok()?);
        let now: u64 = now.as_millis().try_into().ok()?;
        if now.checked_sub(issued)? >= TTL_MILLIS {
            return None;
        }
        Some(quiche::ConnectionId::from_ref(
            token.get(HEADER_LEN..body_len)?,
        ))
    }
}

fn authentication_input(body: &[u8], retry_cid: &[u8], source: &SocketAddr) -> Vec<u8> {
    // The caller has bounded body and retry_cid. Family tags, a fixed-size Retry
    // CID, and the token's explicit ODCID length make this encoding unambiguous.
    let mut input = Vec::with_capacity(DOMAIN.len() + 19 + quiche::MAX_CONN_ID_LEN + body.len());
    input.extend_from_slice(DOMAIN);
    match source.ip() {
        IpAddr::V4(ip) => {
            input.push(4);
            input.extend_from_slice(&ip.octets());
        }
        IpAddr::V6(ip) => {
            input.push(6);
            input.extend_from_slice(&ip.octets());
        }
    }
    input.extend_from_slice(&source.port().to_be_bytes());
    input.extend_from_slice(retry_cid);
    input.extend_from_slice(body);
    input
}

#[cfg(test)]
mod tests;
