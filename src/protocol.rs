use std::io::{self, Read, Write};

const MAGIC: [u8; 4] = *b"DTCH";
const REQUEST_LEN: usize = 13;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct WindowSize {
    pub(crate) rows: u16,
    pub(crate) cols: u16,
    pub(crate) xpixel: u16,
    pub(crate) ypixel: u16,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Request {
    Attach(WindowSize),
    Resize(WindowSize),
}

impl Request {
    /// Serializes one fixed-size client request to the session server.
    pub(crate) fn write_to(self, mut writer: impl Write) -> io::Result<()> {
        let (kind, size) = match self {
            Self::Attach(size) => (1, size),
            Self::Resize(size) => (2, size),
        };
        let mut bytes = [0_u8; REQUEST_LEN];
        bytes[..4].copy_from_slice(&MAGIC);
        bytes[4] = kind;
        bytes[5..7].copy_from_slice(&size.rows.to_be_bytes());
        bytes[7..9].copy_from_slice(&size.cols.to_be_bytes());
        bytes[9..11].copy_from_slice(&size.xpixel.to_be_bytes());
        bytes[11..13].copy_from_slice(&size.ypixel.to_be_bytes());
        writer.write_all(&bytes)
    }

    /// Parses and validates one fixed-size client request from a stream.
    pub(crate) fn read_from(mut reader: impl Read) -> io::Result<Self> {
        let mut bytes = [0_u8; REQUEST_LEN];
        reader.read_exact(&mut bytes)?;
        if bytes[..4] != MAGIC {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid client handshake",
            ));
        }

        let size = WindowSize {
            rows: u16::from_be_bytes([bytes[5], bytes[6]]),
            cols: u16::from_be_bytes([bytes[7], bytes[8]]),
            xpixel: u16::from_be_bytes([bytes[9], bytes[10]]),
            ypixel: u16::from_be_bytes([bytes[11], bytes[12]]),
        };
        match bytes[4] {
            1 => Ok(Self::Attach(size)),
            2 => Ok(Self::Resize(size)),
            _ => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "unknown client request",
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Request, WindowSize};

    #[test]
    /// Verifies that both protocol variants survive serialization and parsing.
    fn request_round_trips() {
        let requests = [
            Request::Attach(WindowSize {
                rows: 24,
                cols: 80,
                xpixel: 640,
                ypixel: 480,
            }),
            Request::Resize(WindowSize {
                rows: 60,
                cols: 160,
                xpixel: 0,
                ypixel: 0,
            }),
        ];

        for request in requests {
            let mut bytes = Vec::new();
            request.write_to(&mut bytes).unwrap();
            assert_eq!(Request::read_from(bytes.as_slice()).unwrap(), request);
        }
    }
}
