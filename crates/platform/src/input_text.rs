//! Bounded Unicode packets shared by native serialized input actors.

pub(crate) fn chunks(units: &[u16]) -> impl Iterator<Item = &[u16]> {
    let mut start = 0;
    std::iter::from_fn(move || {
        if start == units.len() {
            return None;
        }
        let mut end = (start + 20).min(units.len());
        if end < units.len() && (0xd800..=0xdbff).contains(&units[end - 1]) {
            end -= 1;
        }
        let chunk = &units[start..end];
        start = end;
        Some(chunk)
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn packets_preserve_surrogate_pairs_at_boundaries() {
        let text = format!("{}😀中{}", "a".repeat(19), "b".repeat(2048));
        let units: Vec<_> = text.encode_utf16().collect();
        let packets: Vec<_> = super::chunks(&units).collect();
        assert!(
            packets
                .iter()
                .all(|packet| packet.len() <= 20 && String::from_utf16(packet).is_ok())
        );
        assert_eq!(packets.concat(), units);
    }
}
