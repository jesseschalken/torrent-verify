use std::collections::BTreeMap;
use std::fmt::Display;
use std::io;
use std::io::Write;
use std::str::from_utf8;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Bencode {
    Int(i64),
    Bytes(Vec<u8>),
    List(Vec<Bencode>),
    Dict(BTreeMap<Vec<u8>, Bencode>),
}

type BoxedError = Box<dyn std::error::Error + Send + Sync>;

impl Bencode {
    pub fn remove_key<S: AsRef<[u8]> + Display>(&mut self, key: S) -> Result<Bencode, String> {
        match self {
            Bencode::Dict(dict) => {
                if let Some(value) = dict.remove(key.as_ref()) {
                    return Ok(value);
                }
                Err(format!("Key '{key}' not found"))
            }
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
    pub fn encode(&self, out: &mut dyn Write) -> io::Result<()> {
        fn dump_string(out: &mut dyn Write, x: &[u8]) -> io::Result<()> {
            write!(out, "{}", x.len())?;
            out.write(&[b':'])?;
            out.write(x)?;
            Ok(())
        }

        Ok(match self {
            Bencode::Int(x) => {
                out.write(&[b'i'])?;
                write!(out, "{x}")?;
                out.write(&[b'e'])?;
            }
            Bencode::Bytes(x) => {
                dump_string(out, &x)?;
            }
            Bencode::List(x) => {
                out.write(&[b'l'])?;
                for v in x {
                    v.encode(out)?;
                }
                out.write(&[b'e'])?;
            }
            Bencode::Dict(x) => {
                out.write(&[b'd'])?;
                for (k, v) in x {
                    dump_string(out, k)?;
                    v.encode(out)?;
                }
                out.write(&[b'e'])?;
            }
        })
    }

    pub fn decode(bytes: &[u8]) -> Result<(Bencode, &[u8]), BoxedError> {
        match bytes {
            [b'i', rest @ ..] => {
                let (int, rest) = split_at_byte(rest, b'e')?;
                Ok((Bencode::Int(from_utf8(int)?.parse()?), rest))
            }
            bytes @ [b'0'..=b'9', ..] => {
                let (string, rest) = decode_string(bytes)?;
                Ok((Bencode::Bytes(string.to_vec()), rest))
            }
            [b'l', rest @ ..] => {
                let mut rest = rest;
                let mut list = Vec::new();
                loop {
                    if let [b'e', rest @ ..] = rest {
                        break Ok((Bencode::List(list), rest));
                    }
                    let (value, rest2) = Bencode::decode(rest)?;
                    list.push(value);
                    rest = rest2;
                }
            }
            [b'd', rest @ ..] => {
                let mut rest = rest;
                let mut dict = BTreeMap::new();
                loop {
                    if let [b'e', rest @ ..] = rest {
                        break Ok((Bencode::Dict(dict), rest));
                    }
                    let (key, rest2) = decode_string(rest)?;
                    let (value, rest2) = Bencode::decode(rest2)?;
                    dict.insert(key.to_vec(), value);
                    rest = rest2;
                }
            }
            [byte, ..] => Err(format!("Unexpected byte '{byte}'"))?,
            [] => Err("Unexpected end of input")?,
        }
    }
}

fn split_at_byte(bytes: &[u8], byte: u8) -> Result<(&[u8], &[u8]), BoxedError> {
    if let Some(index) = bytes.iter().position(|x| *x == byte) {
        return Ok((&bytes[..index], &bytes[(index + 1)..]));
    }
    Err(format!("Missing byte '{byte}'"))?
}

fn decode_string(bytes: &[u8]) -> Result<(&[u8], &[u8]), BoxedError> {
    let (length, rest) = split_at_byte(bytes, b':')?;
    let mid: usize = from_utf8(length)?.parse()?;
    if mid <= rest.len() {
        return Ok(rest.split_at(mid));
    }
    Err(format!("String length {mid} exceeds remaining bytes"))?
}
