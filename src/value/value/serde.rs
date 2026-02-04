use std::{borrow::Cow, collections::BTreeMap, fmt};

use crate::value::value::{Value, simdutf_bytes_utf8_lossy, timestamp_to_string};
use bytes::Bytes;
use ordered_float::NotNan;
use rust_decimal::Decimal;
use serde::de::Error as SerdeError;
use serde::de::{MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Serialize, Serializer};

impl Value {
    /// Converts self into a `Bytes`, using JSON for Map/Array.
    ///
    /// # Panics
    /// If map or array serialization fails.
    pub fn coerce_to_bytes(&self) -> Bytes {
        match self {
            Self::Bytes(bytes) => bytes.clone(), // cloning `Bytes` is cheap
            Self::Regex(regex) => regex.as_bytes(),
            Self::Timestamp(timestamp) => Bytes::from(timestamp_to_string(timestamp)),
            Self::Integer(num) => Bytes::from(num.to_string()),
            Self::Float(num) => Bytes::from(num.to_string()),
            Self::Decimal(num) => Bytes::from(num.to_string()),
            Self::Boolean(b) => Bytes::from(b.to_string()),
            Self::Object(map) => {
                Bytes::from(serde_json::to_vec(map).expect("Cannot serialize map"))
            }
            Self::Array(arr) => {
                Bytes::from(serde_json::to_vec(arr).expect("Cannot serialize array"))
            }
            Self::Null => Bytes::from("<null>"),
        }
    }

    /// Converts self into a `String` representation, using JSON for `Map`/`Array`.
    ///
    /// # Panics
    /// If map or array serialization fails.
    pub fn to_string_lossy(&self) -> Cow<'_, str> {
        match self {
            Self::Bytes(bytes) => simdutf_bytes_utf8_lossy(bytes),
            Self::Regex(regex) => regex.as_str().into(),
            Self::Timestamp(timestamp) => timestamp_to_string(timestamp).into(),
            Self::Integer(num) => num.to_string().into(),
            Self::Float(num) => num.to_string().into(),
            Self::Decimal(num) => num.to_string().into(),
            Self::Boolean(b) => b.to_string().into(),
            Self::Object(map) => serde_json::to_string(map)
                .expect("Cannot serialize map")
                .into(),
            Self::Array(arr) => serde_json::to_string(arr)
                .expect("Cannot serialize array")
                .into(),
            Self::Null => "<null>".into(),
        }
    }

    /// Recursively converts numeric-looking strings to Integer or Decimal/Float.
    ///
    /// When `use_decimal` is true (default):
    /// 1. Try i64 (fits in standard Integer)
    /// 2. Try Decimal (has decimal point or exceeds i64)
    /// 3. Keep as Bytes (not a valid number)
    ///
    /// When `use_decimal` is false:
    /// 1. Try i64 (fits in standard Integer)
    /// 2. Try f64 (has decimal point or exceeds i64)
    /// 3. Keep as Bytes (not a valid number)
    ///
    /// # Examples
    ///
    /// ```
    /// use vrl::value::Value;
    ///
    /// // Integer promotion
    /// let v = Value::from("123");
    /// assert_eq!(v.parse_numeric(true), Value::Integer(123));
    ///
    /// // Decimal promotion (use_decimal = true)
    /// let v = Value::from("123.456");
    /// assert!(v.parse_numeric(true).is_decimal());
    ///
    /// // Float promotion (use_decimal = false)
    /// let v = Value::from("123.456");
    /// assert!(v.parse_numeric(false).is_float());
    ///
    /// // Non-numeric stays as Bytes
    /// let v = Value::from("hello");
    /// assert!(v.clone().parse_numeric(true).is_bytes());
    /// ```
    #[must_use]
    pub fn parse_numeric(self, use_decimal: bool) -> Self {
        match self {
            Self::Bytes(ref b) => {
                let Ok(s) = std::str::from_utf8(b) else {
                    return self;
                };
                let s = s.trim();

                // Fast bailout: check first character
                let dominated_by_numbers = s
                    .bytes()
                    .next()
                    .is_some_and(|c| matches!(c, b'0'..=b'9' | b'-' | b'+' | b'.'));

                if !dominated_by_numbers {
                    return self;
                }

                // Try integer first (no decimal point)
                if !s.contains('.')
                    && let Ok(n) = s.parse::<i64>()
                {
                    return Self::Integer(n);
                }

                if use_decimal {
                    // Try Decimal
                    if let Ok(d) = s.parse::<Decimal>() {
                        return d.into();
                    }
                } else {
                    // Try Float
                    if let Ok(f) = s.parse::<f64>()
                        && let Ok(not_nan) = NotNan::new(f)
                    {
                        return Self::Float(not_nan);
                    }
                }

                self
            }
            Self::Array(arr) => Self::Array(
                arr.into_iter()
                    .map(|v| v.parse_numeric(use_decimal))
                    .collect(),
            ),
            Self::Object(map) => Self::Object(
                map.into_iter()
                    .map(|(k, v)| (k, v.parse_numeric(use_decimal)))
                    .collect(),
            ),
            other => other,
        }
    }
}

impl Serialize for Value {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match &self {
            Self::Integer(i) => serializer.serialize_i64(*i),
            Self::Float(f) => serializer.serialize_f64(f.into_inner()),
            Self::Decimal(d) => {
                // Serialize as a JSON number preserving full decimal precision.
                // Try integer first for whole numbers, then use RawValue to write
                // the exact decimal representation without an f64 round-trip.
                if d.fract().is_zero()
                    && let Ok(i) = i64::try_from(*d)
                {
                    return serializer.serialize_i64(i);
                }
                let raw = serde_json::value::RawValue::from_string(d.to_string())
                    .expect("Decimal::to_string always produces valid JSON");
                raw.serialize(serializer)
            }
            Self::Boolean(b) => serializer.serialize_bool(*b),
            Self::Bytes(b) => serializer.serialize_str(simdutf_bytes_utf8_lossy(b).as_ref()),
            Self::Timestamp(ts) => serializer.serialize_str(&timestamp_to_string(ts)),
            Self::Regex(regex) => serializer.serialize_str(regex.as_str()),
            Self::Object(m) => serializer.collect_map(m),
            Self::Array(a) => serializer.collect_seq(a),
            Self::Null => serializer.serialize_none(),
        }
    }
}

impl<'de> Deserialize<'de> for Value {
    #[inline]
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct ValueVisitor;

        impl<'de> Visitor<'de> for ValueVisitor {
            type Value = Value;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("any valid JSON value")
            }

            #[inline]
            fn visit_bool<E>(self, value: bool) -> Result<Value, E> {
                Ok(value.into())
            }

            #[inline]
            fn visit_i64<E>(self, value: i64) -> Result<Value, E> {
                Ok(value.into())
            }

            #[inline]
            fn visit_u64<E>(self, value: u64) -> Result<Value, E>
            where
                E: serde::de::Error,
            {
                if let Ok(value) = i64::try_from(value) {
                    Ok(value.into())
                } else {
                    // TODO: Address this issue by providing a lossless conversion option.
                    #[allow(clippy::cast_precision_loss)] //TODO evaluate removal options
                    let converted_value = value as f64;
                    let wrapped_value = NotNan::new(converted_value).map_err(|_| {
                        SerdeError::invalid_value(
                            serde::de::Unexpected::Float(converted_value),
                            &self,
                        )
                    })?;
                    Ok(Value::Float(wrapped_value))
                }
            }

            #[inline]
            fn visit_f64<E>(self, value: f64) -> Result<Value, E>
            where
                E: serde::de::Error,
            {
                let f = NotNan::new(value).map_err(|_| {
                    SerdeError::invalid_value(serde::de::Unexpected::Float(value), &self)
                })?;
                Ok(Value::Float(f))
            }

            #[inline]
            fn visit_str<E>(self, value: &str) -> Result<Value, E>
            where
                E: serde::de::Error,
            {
                Ok(Value::Bytes(Bytes::copy_from_slice(value.as_bytes())))
            }

            #[inline]
            fn visit_string<E>(self, value: String) -> Result<Value, E> {
                Ok(Value::Bytes(value.into()))
            }

            #[inline]
            fn visit_none<E>(self) -> Result<Value, E> {
                Ok(Value::Null)
            }

            #[inline]
            fn visit_some<D>(self, deserializer: D) -> Result<Value, D::Error>
            where
                D: serde::Deserializer<'de>,
            {
                Deserialize::deserialize(deserializer)
            }

            #[inline]
            fn visit_unit<E>(self) -> Result<Value, E> {
                Ok(Value::Null)
            }

            #[inline]
            fn visit_seq<V>(self, mut visitor: V) -> Result<Value, V::Error>
            where
                V: SeqAccess<'de>,
            {
                let mut vec = Vec::new();
                while let Some(value) = visitor.next_element()? {
                    vec.push(value);
                }

                Ok(Value::Array(vec))
            }

            fn visit_map<V>(self, mut visitor: V) -> Result<Value, V::Error>
            where
                V: MapAccess<'de>,
            {
                let mut map = BTreeMap::new();
                while let Some((key, value)) = visitor.next_entry()? {
                    map.insert(key, value);
                }

                Ok(Value::Object(map))
            }
        }

        deserializer.deserialize_any(ValueVisitor)
    }
}

impl From<serde_json::Value> for Value {
    fn from(json_value: serde_json::Value) -> Self {
        match json_value {
            serde_json::Value::Bool(b) => Self::Boolean(b),
            serde_json::Value::Number(n) if n.is_i64() => n.as_i64().unwrap().into(),
            serde_json::Value::Number(n) if n.is_f64() => {
                // JSON doesn't support NaN values
                NotNan::new(n.as_f64().unwrap()).unwrap().into()
            }
            serde_json::Value::Number(n) => n.to_string().into(),
            serde_json::Value::String(s) => Self::Bytes(Bytes::from(s)),
            serde_json::Value::Object(obj) => Self::Object(
                obj.into_iter()
                    .map(|(key, value)| (key.into(), Self::from(value)))
                    .collect(),
            ),
            serde_json::Value::Array(arr) => Self::Array(arr.into_iter().map(Self::from).collect()),
            serde_json::Value::Null => Self::Null,
        }
    }
}

impl From<&serde_json::Value> for Value {
    fn from(json_value: &serde_json::Value) -> Self {
        json_value.clone().into()
    }
}

/// Recursively converts a `serde_json::value::RawValue` to a `Value`, preserving
/// number precision by parsing numeric strings directly as `Integer` or `Decimal`.
impl TryFrom<&serde_json::value::RawValue> for Value {
    type Error = serde_json::Error;

    fn try_from(value: &serde_json::value::RawValue) -> Result<Self, Self::Error> {
        use serde_json::value::RawValue;

        fn parse_number(raw: &str) -> Result<Value, serde_json::Error> {
            if !raw.contains(['.', 'e', 'E'])
                && let Ok(n) = raw.parse::<i64>()
            {
                return Ok(Value::Integer(n));
            }
            raw.parse::<Decimal>()
                .map(Value::from)
                .map_err(|_| serde_json::Error::custom(format!("failed to parse number: {raw}")))
        }

        let raw = value.get();

        match raw.as_bytes()[0] {
            b'{' => serde_json::from_str::<BTreeMap<String, &RawValue>>(raw)?
                .into_iter()
                .map(|(k, v)| Ok((k.into(), Self::try_from(v)?)))
                .collect::<Result<BTreeMap<_, _>, _>>()
                .map(Self::Object),
            b'[' => serde_json::from_str::<Vec<&RawValue>>(raw)?
                .into_iter()
                .map(Self::try_from)
                .collect::<Result<Vec<_>, _>>()
                .map(Self::Array),
            b'"' => serde_json::from_str::<String>(raw).map(|s| Self::Bytes(s.into())),
            b't' | b'f' => serde_json::from_str::<bool>(raw).map(Self::Boolean),
            b'n' => Ok(Self::Null),
            _ => parse_number(raw),
        }
    }
}

#[cfg(test)]
mod test {
    use std::fs;
    use std::io::Read;
    use std::path::Path;

    use crate::value::Value;

    pub fn parse_artifact(path: impl AsRef<Path>) -> std::io::Result<Vec<u8>> {
        let mut test_file = fs::File::open(path)?;

        let mut buf = Vec::new();
        test_file.read_to_end(&mut buf)?;

        Ok(buf)
    }

    // This test iterates over the `tests/data/fixtures/value` folder and:
    //   * Ensures the parsed folder name matches the parsed type of the `Value`.
    //   * Ensures the `serde_json::Value` to `vector::Value` conversions are harmless. (Think UTF-8 errors)
    //
    // Basically: This test makes sure we aren't mutilating any content users might be sending.
    #[test]
    fn json_value_to_vector_value_to_json_value() {
        const FIXTURE_ROOT: &str = "tests/data/fixtures/value";

        for type_dir in std::fs::read_dir(FIXTURE_ROOT).unwrap() {
            type_dir.map_or_else(
                |_| panic!("This test should never read Err'ing type folders."),
                |type_name| {
                    let path = type_name.path();
                    for fixture_file in std::fs::read_dir(path).unwrap() {
                        fixture_file.map_or_else(
                            |_| panic!("This test should never read Err'ing test fixtures."),
                            |fixture_file| {
                                let path = fixture_file.path();
                                let buf = parse_artifact(path).unwrap();

                                let serde_value: serde_json::Value =
                                    serde_json::from_slice(&buf).unwrap();
                                let vector_value = Value::from(serde_value);

                                // Validate type
                                let expected_type = type_name
                                    .path()
                                    .file_name()
                                    .unwrap()
                                    .to_string_lossy()
                                    .to_string();
                                let is_match = match vector_value {
                                    Value::Boolean(_) => expected_type.eq("boolean"),
                                    Value::Integer(_) => expected_type.eq("integer"),
                                    Value::Float(_) => expected_type.eq("float"),
                                    Value::Decimal(_) => expected_type.eq("decimal"),
                                    Value::Bytes(_) => expected_type.eq("bytes"),
                                    Value::Array { .. } => expected_type.eq("array"),
                                    Value::Object(_) => expected_type.eq("map"),
                                    Value::Null => expected_type.eq("null"),
                                    Value::Timestamp(_) => expected_type.eq("timestamp"),
                                    Value::Regex(_) => expected_type.eq("regex"),
                                };
                                assert!(
                                    is_match,
                                    "Typecheck failure. Wanted {expected_type}, got {vector_value:?}."
                                );
                                // Validate that the value can be serialized back to JSON
                                // via the Serialize impl (which preserves decimal precision).
                                let _json = serde_json::to_string(&vector_value).unwrap();
                            },
                        );
                    }
                },
            );
        }
    }

    #[test]
    fn serialize_decimal_preserves_precision() {
        use rust_decimal::Decimal;
        use std::str::FromStr;

        // 28 significant digits — exceeds f64's ~15-17 digit precision
        let d = Decimal::from_str("1234567890.1234567890123456789").unwrap();
        let value = Value::Decimal(d);

        let json = serde_json::to_string(&value).unwrap();
        assert_eq!(json, "1234567890.1234567890123456789");
    }

    #[test]
    fn decimal_nested_in_object_round_trips_via_raw_value() {
        use rust_decimal::Decimal;
        use serde_json::value::RawValue;
        use std::str::FromStr;

        let d = Decimal::from_str("99.95").unwrap();
        let value = Value::Object(
            vec![
                ("price".into(), Value::Decimal(d)),
                ("qty".into(), Value::Integer(3)),
            ]
            .into_iter()
            .collect(),
        );

        // Serialize preserves decimal precision in JSON output
        let json = serde_json::to_string(&value).unwrap();
        assert!(json.contains("99.95"));

        // Round-trip through RawValue preserves Decimal type
        let raw: Box<RawValue> = serde_json::from_str(&json).unwrap();
        let parsed = Value::try_from(raw.as_ref()).unwrap();

        let obj = parsed.as_object().unwrap();
        assert!(obj.get("price").unwrap().is_decimal());
        assert_eq!(
            obj.get("price").unwrap().as_decimal().unwrap(),
            &Decimal::from_str("99.95").unwrap()
        );
        assert_eq!(obj.get("qty").unwrap(), &Value::Integer(3));
    }

    #[test]
    fn decimal_nested_in_array_round_trips_via_raw_value() {
        use rust_decimal::Decimal;
        use serde_json::value::RawValue;
        use std::str::FromStr;

        let d = Decimal::from_str("1.0000000000000000001").unwrap();
        let value = Value::Array(vec![Value::Integer(1), Value::Decimal(d), Value::Null]);

        let json = serde_json::to_string(&value).unwrap();
        let raw: Box<RawValue> = serde_json::from_str(&json).unwrap();
        let parsed = Value::try_from(raw.as_ref()).unwrap();

        let arr = parsed.as_array().unwrap();
        assert_eq!(arr[0], Value::Integer(1));
        assert!(arr[1].is_decimal());
        assert_eq!(
            arr[1].as_decimal().unwrap(),
            &Decimal::from_str("1.0000000000000000001").unwrap()
        );
        assert_eq!(arr[2], Value::Null);
    }

    #[test]
    fn serialize_decimal_beyond_f64_precision() {
        use rust_decimal::Decimal;
        use std::str::FromStr;

        // 28 significant digits — more than f64 can represent
        let d = Decimal::from_str("9999999999999999999999999999").unwrap();
        let value = Value::Decimal(d);

        let json = serde_json::to_string(&value).unwrap();
        assert_eq!(json, "9999999999999999999999999999");

        // Verify f64 would lose precision here
        let as_f64: f64 = 9999999999999999999999999999.0;
        assert_ne!(as_f64.to_string(), "9999999999999999999999999999");
    }

    #[test]
    fn deserialize_large_decimal_via_raw_value() {
        use rust_decimal::Decimal;
        use serde_json::value::RawValue;
        use std::str::FromStr;

        // Decimal::MAX has 28 nines
        let raw: Box<RawValue> = serde_json::from_str("79228162514264337593543950335").unwrap();
        let value = Value::try_from(raw.as_ref()).unwrap();

        assert!(value.is_decimal());
        assert_eq!(
            value.as_decimal().unwrap(),
            &Decimal::from_str("79228162514264337593543950335").unwrap()
        );
    }

    mod parse_numeric {
        use super::*;
        use rust_decimal::dec;
        use std::collections::BTreeMap;

        #[test]
        fn integer_promotion() {
            let v = Value::from("123");
            assert_eq!(v.parse_numeric(true), Value::Integer(123));
        }

        #[test]
        fn negative_integer_promotion() {
            let v = Value::from("-456");
            assert_eq!(v.parse_numeric(true), Value::Integer(-456));
        }

        #[test]
        fn decimal_promotion() {
            let v = Value::from("123.456");
            let promoted = v.parse_numeric(true);
            assert!(promoted.is_decimal());
            assert_eq!(promoted.as_decimal().unwrap(), &dec!(123.456));
        }

        #[test]
        fn negative_decimal_promotion() {
            let v = Value::from("-99.95");
            let promoted = v.parse_numeric(true);
            assert!(promoted.is_decimal());
            assert_eq!(promoted.as_decimal().unwrap(), &dec!(-99.95));
        }

        #[test]
        fn float_promotion_when_decimal_false() {
            let v = Value::from("123.456");
            let promoted = v.parse_numeric(false);
            assert!(promoted.is_float());
            assert_eq!(promoted.as_float().unwrap(), 123.456);
        }

        #[test]
        fn non_numeric_stays_bytes() {
            let v = Value::from("hello");
            let promoted = v.clone().parse_numeric(true);
            assert!(promoted.is_bytes());
            assert_eq!(promoted, v);
        }

        #[test]
        fn whitespace_trimmed() {
            let v = Value::from("  123  ");
            assert_eq!(v.parse_numeric(true), Value::Integer(123));
        }

        #[test]
        fn nested_array_promotion() {
            let v = Value::Array(vec![
                Value::from("123"),
                Value::from("45.67"),
                Value::from("hello"),
            ]);
            let promoted = v.parse_numeric(true);
            match promoted {
                Value::Array(arr) => {
                    assert_eq!(arr[0], Value::Integer(123));
                    assert!(arr[1].is_decimal());
                    assert!(arr[2].is_bytes());
                }
                _ => panic!("Expected array"),
            }
        }

        #[test]
        fn nested_array_promotion_with_float() {
            let v = Value::Array(vec![
                Value::from("123"),
                Value::from("45.67"),
                Value::from("hello"),
            ]);
            let promoted = v.parse_numeric(false);
            match promoted {
                Value::Array(arr) => {
                    assert_eq!(arr[0], Value::Integer(123));
                    assert!(arr[1].is_float());
                    assert!(arr[2].is_bytes());
                }
                _ => panic!("Expected array"),
            }
        }

        #[test]
        fn nested_object_promotion() {
            let mut map = BTreeMap::new();
            map.insert("amount".into(), Value::from("99.95"));
            map.insert("count".into(), Value::from("42"));
            map.insert("name".into(), Value::from("test"));
            let v = Value::Object(map);

            let promoted = v.parse_numeric(true);
            match promoted {
                Value::Object(map) => {
                    assert!(map.get("amount").unwrap().is_decimal());
                    assert_eq!(*map.get("count").unwrap(), Value::Integer(42));
                    assert!(map.get("name").unwrap().is_bytes());
                }
                _ => panic!("Expected object"),
            }
        }

        #[test]
        fn other_types_unchanged() {
            // Integer stays integer
            let v = Value::Integer(123);
            assert_eq!(v.clone().parse_numeric(true), v);

            // Boolean stays boolean
            let v = Value::Boolean(true);
            assert_eq!(v.clone().parse_numeric(true), v);

            // Null stays null
            let v = Value::Null;
            assert_eq!(v.clone().parse_numeric(true), v);

            // Decimal stays decimal
            let v = Value::Decimal(dec!(1.23));
            assert_eq!(v.clone().parse_numeric(true), v);
        }

        #[test]
        fn large_integer_becomes_decimal() {
            // i64::MAX + 1 cannot fit in i64, so becomes decimal
            let v = Value::from("9223372036854775808");
            let promoted = v.parse_numeric(true);
            assert!(promoted.is_decimal());
        }

        #[test]
        fn large_integer_becomes_float_when_decimal_false() {
            // i64::MAX + 1 cannot fit in i64, so becomes float when decimal=false
            let v = Value::from("9223372036854775808");
            let promoted = v.parse_numeric(false);
            assert!(promoted.is_float());
        }

        #[test]
        fn leading_plus_sign() {
            let v = Value::from("+123");
            assert_eq!(v.parse_numeric(true), Value::Integer(123));
        }

        #[test]
        fn decimal_with_leading_dot() {
            let v = Value::from(".5");
            let promoted = v.parse_numeric(true);
            assert!(promoted.is_decimal());
        }
    }

    mod from_raw_json {
        use super::*;
        use rust_decimal::Decimal;
        use serde_json::value::RawValue;

        fn parse_raw(s: &str) -> Value {
            let raw: Box<RawValue> = serde_json::from_str(s).unwrap();
            Value::try_from(raw.as_ref()).unwrap()
        }

        // --- Primitives ---

        #[test]
        fn null() {
            assert_eq!(parse_raw("null"), Value::Null);
        }

        #[test]
        fn bool_true() {
            assert_eq!(parse_raw("true"), Value::Boolean(true));
        }

        #[test]
        fn bool_false() {
            assert_eq!(parse_raw("false"), Value::Boolean(false));
        }

        #[test]
        fn simple_string() {
            assert_eq!(parse_raw(r#""hello""#), Value::from("hello"));
        }

        #[test]
        fn string_with_escapes() {
            assert_eq!(parse_raw(r#""line1\nline2""#), Value::from("line1\nline2"));
        }

        #[test]
        fn string_with_unicode_escape() {
            assert_eq!(parse_raw(r#""\u0041""#), Value::from("A"));
        }

        #[test]
        fn empty_string() {
            assert_eq!(parse_raw(r#""""#), Value::from(""));
        }

        // --- Integers ---

        #[test]
        fn zero() {
            assert_eq!(parse_raw("0"), Value::Integer(0));
        }

        #[test]
        fn positive_integer() {
            assert_eq!(parse_raw("42"), Value::Integer(42));
        }

        #[test]
        fn negative_integer() {
            assert_eq!(parse_raw("-7"), Value::Integer(-7));
        }

        #[test]
        fn i64_max() {
            let s = i64::MAX.to_string();
            assert_eq!(parse_raw(&s), Value::Integer(i64::MAX));
        }

        #[test]
        fn i64_min() {
            let s = i64::MIN.to_string();
            assert_eq!(parse_raw(&s), Value::Integer(i64::MIN));
        }

        #[test]
        fn i64_overflow_becomes_decimal() {
            let val = parse_raw("9223372036854775808");
            assert!(val.is_decimal());
            assert_eq!(
                *val.as_decimal().unwrap(),
                "9223372036854775808".parse::<Decimal>().unwrap()
            );
        }

        #[test]
        fn i64_underflow_becomes_decimal() {
            let val = parse_raw("-9223372036854775809");
            assert!(val.is_decimal());
            assert_eq!(
                *val.as_decimal().unwrap(),
                "-9223372036854775809".parse::<Decimal>().unwrap()
            );
        }

        // --- Decimals ---

        #[test]
        fn simple_decimal() {
            let val = parse_raw("3.14");
            assert!(val.is_decimal());
            assert_eq!(
                *val.as_decimal().unwrap(),
                "3.14".parse::<Decimal>().unwrap()
            );
        }

        #[test]
        fn negative_decimal() {
            let val = parse_raw("-0.5");
            assert!(val.is_decimal());
            assert_eq!(
                *val.as_decimal().unwrap(),
                "-0.5".parse::<Decimal>().unwrap()
            );
        }

        #[test]
        fn decimal_with_trailing_zeros() {
            let val = parse_raw("1.200");
            assert!(val.is_decimal());
            assert_eq!(
                *val.as_decimal().unwrap(),
                "1.200".parse::<Decimal>().unwrap()
            );
        }

        #[test]
        fn high_precision_decimal() {
            let val = parse_raw("0.12379999458789825");
            assert!(val.is_decimal());
            assert_eq!(
                *val.as_decimal().unwrap(),
                "0.12379999458789825".parse::<Decimal>().unwrap()
            );
        }

        // --- Exponent notation ---

        #[test]
        fn exponent_notation_errors() {
            let raw: Box<RawValue> = serde_json::from_str("1e2").unwrap();
            let result = Value::try_from(raw.as_ref());
            assert!(result.is_err());
        }

        #[test]
        fn uppercase_exponent_errors() {
            let raw: Box<RawValue> = serde_json::from_str("1E2").unwrap();
            let result = Value::try_from(raw.as_ref());
            assert!(result.is_err());
        }

        #[test]
        fn negative_exponent_errors() {
            let raw: Box<RawValue> = serde_json::from_str("5e-3").unwrap();
            let result = Value::try_from(raw.as_ref());
            assert!(result.is_err());
        }

        // --- Objects ---

        #[test]
        fn empty_object() {
            let val = parse_raw("{}");
            assert!(val.is_object());
            assert!(val.as_object().unwrap().is_empty());
        }

        #[test]
        fn simple_object() {
            let val = parse_raw(r#"{"key": "value"}"#);
            let obj = val.as_object().unwrap();
            assert_eq!(obj.get("key").unwrap(), &Value::from("value"));
        }

        #[test]
        fn object_with_mixed_types() {
            let val = parse_raw(r#"{"s": "str", "n": 1, "f": 1.5, "b": true, "nil": null}"#);
            let obj = val.as_object().unwrap();
            assert_eq!(obj.get("s").unwrap(), &Value::from("str"));
            assert_eq!(obj.get("n").unwrap(), &Value::Integer(1));
            assert!(obj.get("f").unwrap().is_decimal());
            assert_eq!(obj.get("b").unwrap(), &Value::Boolean(true));
            assert_eq!(obj.get("nil").unwrap(), &Value::Null);
        }

        #[test]
        fn nested_objects() {
            let val = parse_raw(r#"{"a": {"b": {"c": 1}}}"#);
            let c = val
                .as_object()
                .unwrap()
                .get("a")
                .unwrap()
                .as_object()
                .unwrap()
                .get("b")
                .unwrap()
                .as_object()
                .unwrap()
                .get("c")
                .unwrap();
            assert_eq!(*c, Value::Integer(1));
        }

        // --- Arrays ---

        #[test]
        fn empty_array() {
            let val = parse_raw("[]");
            assert!(val.is_array());
            assert!(val.as_array().unwrap().is_empty());
        }

        #[test]
        fn array_of_integers() {
            let val = parse_raw("[1, 2, 3]");
            let arr = val.as_array().unwrap();
            assert_eq!(
                arr,
                &[Value::Integer(1), Value::Integer(2), Value::Integer(3)]
            );
        }

        #[test]
        fn array_with_mixed_types() {
            let val = parse_raw(r#"[1, "two", 3.0, true, null]"#);
            let arr = val.as_array().unwrap();
            assert_eq!(arr[0], Value::Integer(1));
            assert_eq!(arr[1], Value::from("two"));
            assert!(arr[2].is_decimal());
            assert_eq!(arr[3], Value::Boolean(true));
            assert_eq!(arr[4], Value::Null);
        }

        #[test]
        fn nested_arrays() {
            let val = parse_raw("[[1, 2], [3, [4]]]");
            let outer = val.as_array().unwrap();
            let inner0 = outer[0].as_array().unwrap();
            assert_eq!(inner0, &[Value::Integer(1), Value::Integer(2)]);
            let inner1 = outer[1].as_array().unwrap();
            assert_eq!(inner1[0], Value::Integer(3));
            let deep = inner1[1].as_array().unwrap();
            assert_eq!(deep, &[Value::Integer(4)]);
        }

        #[test]
        fn array_of_objects() {
            let val = parse_raw(r#"[{"a": 1}, {"b": 2}]"#);
            let arr = val.as_array().unwrap();
            assert_eq!(
                arr[0].as_object().unwrap().get("a").unwrap(),
                &Value::Integer(1)
            );
            assert_eq!(
                arr[1].as_object().unwrap().get("b").unwrap(),
                &Value::Integer(2)
            );
        }
    }
}
