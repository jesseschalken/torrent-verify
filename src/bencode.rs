use atoi::FromRadix10;
use atoi::FromRadix10Signed;
use std::collections::BTreeMap;
use std::error::Error;
use std::io;
use std::io::Write;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Bencode {
    Int(i64),
    Bytes(Vec<u8>),
    List(Vec<Bencode>),
    Dict(BTreeMap<Vec<u8>, Bencode>),
}

type BoxedError = Box<dyn Error + Send + Sync>;

impl Bencode {
    pub fn remove_key(&mut self, key: &str) -> Result<Bencode, String> {
        match self {
            Bencode::Dict(dict) => dict
                .remove(key.as_bytes())
                .ok_or_else(|| format!("Key '{key}' not found")),
            _ => Err(format!("Expected a dict, got {}", self.get_type())),
        }
    }

    pub fn get_type(&self) -> &str {
        match self {
            Bencode::Int(_) => "int",
            Bencode::Bytes(_) => "bytes",
            Bencode::List(_) => "list",
            Bencode::Dict(_) => "dict",
        }
    }

    pub fn try_into_list(self) -> Result<Vec<Bencode>, String> {
        match self {
            Bencode::List(x) => Ok(x),
            _ => Err(format!("Expected a list, got {}", self.get_type())),
        }
    }

    pub fn try_into_int(self) -> Result<i64, String> {
        match self {
            Bencode::Int(x) => Ok(x),
            _ => Err(format!("Expected an int, got {}", self.get_type())),
        }
    }

    pub fn try_into_bytes(self) -> Result<Vec<u8>, String> {
        match self {
            Bencode::Bytes(x) => Ok(x),
            _ => Err(format!("Expected bytes, got {}", self.get_type())),
        }
    }

    pub fn try_into_dict(self) -> Result<BTreeMap<Vec<u8>, Bencode>, String> {
        match self {
            Bencode::Dict(x) => Ok(x),
            _ => Err(format!("Expected a dict, got {}", self.get_type())),
        }
    }
}

impl TryFrom<Bencode> for i64 {
    type Error = String;
    fn try_from(value: Bencode) -> Result<Self, Self::Error> {
        value.try_into_int()
    }
}

impl TryFrom<Bencode> for usize {
    type Error = BoxedError;
    fn try_from(value: Bencode) -> Result<Self, Self::Error> {
        Ok(value.try_into_int()?.try_into()?)
    }
}

impl TryFrom<Bencode> for Vec<u8> {
    type Error = String;
    fn try_from(value: Bencode) -> Result<Self, Self::Error> {
        value.try_into_bytes()
    }
}

impl TryFrom<Bencode> for String {
    type Error = BoxedError;
    fn try_from(value: Bencode) -> Result<Self, Self::Error> {
        Ok(value.try_into_bytes()?.try_into()?)
    }
}

impl<T: TryFrom<Bencode>> TryFrom<Bencode> for Vec<T>
where
    BoxedError: From<T::Error>,
{
    type Error = BoxedError;
    fn try_from(value: Bencode) -> Result<Self, Self::Error> {
        Ok(value
            .try_into_list()?
            .into_iter()
            .map(T::try_from)
            .collect::<Result<_, _>>()?)
    }
}

impl TryFrom<Bencode> for BTreeMap<Vec<u8>, Bencode> {
    type Error = String;
    fn try_from(value: Bencode) -> Result<Self, Self::Error> {
        value.try_into_dict()
    }
}

impl Bencode {
    pub fn encode_to_vec(&self) -> Vec<u8> {
        let mut vec = Vec::new();
        self.encode(&mut vec).unwrap();
        vec
    }

    pub fn encode(&self, out: &mut dyn Write) -> io::Result<()> {
        fn dump_string(out: &mut dyn Write, x: &[u8]) -> io::Result<()> {
            out.write_all(itoa::Buffer::new().format(x.len()).as_bytes())?;
            out.write_all(b":")?;
            out.write_all(x)?;
            Ok(())
        }

        Ok(match self {
            Bencode::Int(x) => {
                out.write_all(b"i")?;
                out.write_all(itoa::Buffer::new().format(*x).as_bytes())?;
                out.write_all(b"e")?;
            }
            Bencode::Bytes(x) => {
                dump_string(out, x)?;
            }
            Bencode::List(x) => {
                out.write_all(b"l")?;
                for v in x {
                    v.encode(out)?;
                }
                out.write_all(b"e")?;
            }
            Bencode::Dict(x) => {
                out.write_all(b"d")?;
                for (k, v) in x {
                    dump_string(out, k)?;
                    v.encode(out)?;
                }
                out.write_all(b"e")?;
            }
        })
    }

    pub fn decode(bytes: &[u8]) -> Result<(Bencode, &[u8]), BoxedError> {
        match bytes {
            [b'i', bytes @ ..] => {
                let (int, pos) = i64::from_radix_10_signed(bytes);
                if let [b'e', bytes @ ..] = &bytes[pos..] {
                    Ok((Bencode::Int(int), bytes))
                } else {
                    Err(format!("Missing 'e' byte after int {int}"))?
                }
            }
            bytes @ [b'0'..=b'9', ..] => {
                let (string, bytes) = decode_string(bytes)?;
                Ok((Bencode::Bytes(string.to_vec()), bytes))
            }
            [b'l', bytes @ ..] => {
                let mut bytes = bytes;
                let mut list = Vec::new();
                loop {
                    if let [b'e', bytes @ ..] = bytes {
                        break Ok((Bencode::List(list), bytes));
                    }
                    let value;
                    (value, bytes) = Bencode::decode(bytes)?;
                    list.push(value);
                }
            }
            [b'd', bytes @ ..] => {
                let mut bytes = bytes;
                let mut dict = BTreeMap::new();
                loop {
                    if let [b'e', bytes @ ..] = bytes {
                        break Ok((Bencode::Dict(dict), bytes));
                    }
                    let key;
                    let val;
                    (key, bytes) = decode_string(bytes)?;
                    (val, bytes) = Bencode::decode(bytes)?;
                    dict.insert(key.to_vec(), val);
                }
            }
            [byte, ..] => Err(format!("Unexpected byte '{byte}'"))?,
            [] => Err("Unexpected end of input")?,
        }
    }
}

fn decode_string(bytes: &[u8]) -> Result<(&[u8], &[u8]), BoxedError> {
    let (len, pos) = usize::from_radix_10(bytes);
    let [b':', bytes @ ..] = &bytes[pos..] else {
        Err("Missing ':' byte")?
    };
    let Some(pair) = bytes.split_at_checked(len) else {
        Err(format!("String length {len} exceeds remaining bytes"))?
    };
    Ok(pair)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check_encode_decode(bytes: &[u8], value: Bencode, trailing: &[u8]) {
        assert_eq!(Bencode::decode(bytes).unwrap(), (value.clone(), trailing));
        let mut encoded = value.encode_to_vec();
        encoded.extend_from_slice(trailing);
        assert_eq!(encoded, bytes);
    }

    #[test]
    fn test_decode_int() {
        check_encode_decode(b"i42e", Bencode::Int(42), b"");
        check_encode_decode(b"i-5eTAIL", Bencode::Int(-5), b"TAIL");

        assert!(Bencode::decode(b"i42").is_err()); // missing 'e'
    }

    #[test]
    fn test_decode_bytes_and_remainder() {
        check_encode_decode(b"4:spam", Bencode::Bytes(b"spam".to_vec()), b"");
        check_encode_decode(b"4:spamXYZ", Bencode::Bytes(b"spam".to_vec()), b"XYZ");

        assert!(Bencode::decode(b"4spam").is_err()); // missing ':'
    }

    #[test]
    fn test_decode_list() {
        check_encode_decode(
            b"l4:spam4:eggse",
            Bencode::List(vec![
                Bencode::Bytes(b"spam".to_vec()),
                Bencode::Bytes(b"eggs".to_vec()),
            ]),
            b"",
        );
    }

    #[test]
    fn test_decode_dict() {
        check_encode_decode(
            b"d3:cow3:moo4:spam4:eggse",
            Bencode::Dict(BTreeMap::from_iter([
                (b"cow".to_vec(), Bencode::Bytes(b"moo".to_vec())),
                (b"spam".to_vec(), Bencode::Bytes(b"eggs".to_vec())),
            ])),
            b"",
        );
    }

    #[test]
    fn test_decode_nested() {
        check_encode_decode(
            b"d4:dictd3:foo3:bare4:listli1ei2ei3eee",
            Bencode::Dict(BTreeMap::from_iter([
                (
                    b"dict".to_vec(),
                    Bencode::Dict(BTreeMap::from_iter([(
                        b"foo".to_vec(),
                        Bencode::Bytes(b"bar".to_vec()),
                    )])),
                ),
                (
                    b"list".to_vec(),
                    Bencode::List(vec![Bencode::Int(1), Bencode::Int(2), Bencode::Int(3)]),
                ),
            ])),
            b"",
        );
    }

    #[test]
    fn test_errors_unexpected_byte_and_eof() {
        assert!(Bencode::decode(b"x").is_err());
        assert!(Bencode::decode(b"").is_err());
    }
}
