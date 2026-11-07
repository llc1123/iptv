use super::RenderError;

pub(super) struct Writer {
    limit: usize,
    pub(super) count: usize,
    output: Option<Vec<u8>>,
}

impl Writer {
    pub(super) fn measure(limit: usize) -> Self {
        Self {
            limit,
            count: 0,
            output: None,
        }
    }
    pub(super) fn output(limit: usize, capacity: usize) -> Result<Self, RenderError> {
        let mut output = Vec::new();
        output
            .try_reserve_exact(capacity)
            .map_err(|_| RenderError::Oversize)?;
        Ok(Self {
            limit,
            count: 0,
            output: Some(output),
        })
    }
    pub(super) fn text(&mut self, text: &str, _: bool) -> Result<(), RenderError> {
        self.bytes(text.as_bytes())
    }
    pub(super) fn line(&mut self, text: &str, newline: bool) -> Result<(), RenderError> {
        self.text(text, false)?;
        if newline {
            self.bytes(b"\n")?;
        }
        Ok(())
    }
    pub(super) fn bytes(&mut self, bytes: &[u8]) -> Result<(), RenderError> {
        self.count = self
            .count
            .checked_add(bytes.len())
            .ok_or(RenderError::Oversize)?;
        if self.count > self.limit {
            return Err(RenderError::Oversize);
        }
        if let Some(output) = &mut self.output {
            output
                .try_reserve(bytes.len())
                .map_err(|_| RenderError::Oversize)?;
            output.extend_from_slice(bytes);
        }
        Ok(())
    }
    pub(super) fn finish(self) -> Result<Vec<u8>, RenderError> {
        self.output.ok_or(RenderError::CacheUnavailable)
    }
}

pub(super) fn validate_line_fields(
    channel: &crate::epg::models::BootChannel,
) -> Result<(), RenderError> {
    for value in [
        &channel.channel_id,
        &channel.name,
        &channel.group_title,
        &channel.logo_url,
    ] {
        if value.bytes().any(|byte| matches!(byte, b'\r' | b'\n' | 0)) {
            return Err(RenderError::InvalidField);
        }
    }
    Ok(())
}

pub(super) fn validate_xml_value(value: &str) -> Result<(), RenderError> {
    if value.contains('\0') {
        Err(RenderError::InvalidField)
    } else {
        Ok(())
    }
}

pub(super) fn write_component(writer: &mut Writer, value: &str) -> Result<(), RenderError> {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    for &byte in value.as_bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            writer.bytes(&[byte])?;
        } else {
            writer.bytes(&[
                b'%',
                HEX[usize::from(byte >> 4)],
                HEX[usize::from(byte & 15)],
            ])?;
        }
    }
    Ok(())
}

pub(super) fn xml_text(writer: &mut Writer, value: &str) -> Result<(), RenderError> {
    for character in value.chars() {
        let mut encoded = [0; 4];
        let text = match character {
            '&' => "&amp;",
            '<' => "&lt;",
            '>' => "&gt;",
            _ => character.encode_utf8(&mut encoded),
        };
        writer.text(text, false)?;
    }
    Ok(())
}

pub(super) fn xml_attr(writer: &mut Writer, value: &str) -> Result<(), RenderError> {
    for character in value.chars() {
        let mut encoded = [0; 4];
        let text = match character {
            '&' => "&amp;",
            '<' => "&lt;",
            '>' => "&gt;",
            '"' => "&quot;",
            '\r' => "&#13;",
            '\n' => "&#10;",
            '\t' => "&#09;",
            _ => character.encode_utf8(&mut encoded),
        };
        writer.text(text, false)?;
    }
    Ok(())
}
