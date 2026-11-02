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

    pub(super) fn text(&mut self, text: &str, _line: bool) -> Result<(), RenderError> {
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
    [
        &channel.channel_id,
        &channel.name,
        &channel.group_title,
        &channel.logo_url,
    ]
    .into_iter()
    .try_for_each(|value| {
        if value.bytes().any(|byte| matches!(byte, b'\r' | b'\n' | 0)) {
            Err(RenderError::InvalidField)
        } else {
            Ok(())
        }
    })
}

pub(super) fn validate_xml_value(value: &str) -> Result<(), RenderError> {
    if value.contains('\0') {
        Err(RenderError::InvalidField)
    } else {
        Ok(())
    }
}

pub(super) fn write_component(writer: &mut Writer, value: &str) -> Result<(), RenderError> {
    let mut encoded = Vec::new();
    append_component(&mut encoded, value)?;
    writer.bytes(&encoded)
}

pub(super) fn append_component(output: &mut Vec<u8>, value: &str) -> Result<(), RenderError> {
    for byte in value.as_bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            output.try_reserve(1).map_err(|_| RenderError::Oversize)?;
            output.push(*byte);
        } else {
            output.try_reserve(3).map_err(|_| RenderError::Oversize)?;
            output.extend_from_slice(b"%00");
            let index = output.len().checked_sub(2).ok_or(RenderError::Oversize)?;
            let digits = b"0123456789ABCDEF";
            output[index] = digits[usize::from(byte >> 4)];
            output[index + 1] = digits[usize::from(byte & 0x0f)];
        }
    }
    Ok(())
}

pub(super) fn xml_text(writer: &mut Writer, value: &str) -> Result<(), RenderError> {
    for character in value.chars() {
        match character {
            '&' => writer.text("&amp;", false)?,
            '<' => writer.text("&lt;", false)?,
            '>' => writer.text("&gt;", false)?,
            _ => writer.text(character.encode_utf8(&mut [0; 4]), false)?,
        }
    }
    Ok(())
}

pub(super) fn xml_attr(writer: &mut Writer, value: &str) -> Result<(), RenderError> {
    for character in value.chars() {
        match character {
            '&' => writer.text("&amp;", false)?,
            '<' => writer.text("&lt;", false)?,
            '>' => writer.text("&gt;", false)?,
            '"' => writer.text("&quot;", false)?,
            '\r' => writer.text("&#13;", false)?,
            '\n' => writer.text("&#10;", false)?,
            '\t' => writer.text("&#09;", false)?,
            _ => writer.text(character.encode_utf8(&mut [0; 4]), false)?,
        }
    }
    Ok(())
}
