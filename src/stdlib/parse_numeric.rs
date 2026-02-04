use crate::compiler::prelude::*;

#[derive(Clone, Copy, Debug)]
pub struct ParseNumeric;

impl Function for ParseNumeric {
    fn identifier(&self) -> &'static str {
        "parse_numeric"
    }

    fn usage(&self) -> &'static str {
        "Recursively converts numeric-looking strings to integers or decimals/floats."
    }

    fn parameters(&self) -> &'static [Parameter] {
        &[
            Parameter {
                keyword: "value",
                kind: kind::ANY,
                required: true,
                description: "The value to process.",
            },
            Parameter {
                keyword: "decimal",
                kind: kind::BOOLEAN,
                required: false,
                description: "Whether to use decimal (true) or float (false) for non-integer numbers. Defaults to true.",
            },
        ]
    }

    fn examples(&self) -> &'static [Example] {
        &[
            example! {
                title: "Integer string",
                source: r#"parse_numeric("123")"#,
                result: Ok("123"),
            },
            example! {
                title: "Decimal string",
                source: r#"parse_numeric("123.456")"#,
                result: Ok("d'123.456'"),
            },
            example! {
                title: "Float string (decimal: false)",
                source: r#"parse_numeric("123.456", decimal: false)"#,
                result: Ok("123.456"),
            },
            example! {
                title: "Non-numeric string",
                source: r#"parse_numeric("hello")"#,
                result: Ok(r#""hello""#),
            },
            example! {
                title: "Nested object",
                source: r#"parse_numeric({"amount": "99.95", "name": "test"})"#,
                result: Ok(r#"{ "amount": d'99.95', "name": "test" }"#),
            },
            example! {
                title: "Nested array",
                source: r#"parse_numeric(["123", "45.67", "hello"])"#,
                result: Ok(r#"[123, d'45.67', "hello"]"#),
            },
        ]
    }

    fn compile(
        &self,
        _state: &state::TypeState,
        _ctx: &mut FunctionCompileContext,
        arguments: ArgumentList,
    ) -> Compiled {
        let value = arguments.required("value");
        let decimal = arguments.optional("decimal").unwrap_or(expr!(true));

        Ok(ParseNumericFn { value, decimal }.as_expr())
    }
}

#[derive(Clone, Debug)]
struct ParseNumericFn {
    value: Box<dyn Expression>,
    decimal: Box<dyn Expression>,
}

impl FunctionExpression for ParseNumericFn {
    fn resolve(&self, ctx: &mut Context) -> Resolved {
        let value = self.value.resolve(ctx)?;
        let use_decimal = self.decimal.resolve(ctx)?.try_boolean()?;
        Ok(value.parse_numeric(use_decimal))
    }

    fn type_def(&self, state: &state::TypeState) -> TypeDef {
        let td = self.value.type_def(state);

        // The result type can be:
        // - Integer (if bytes that look like integers)
        // - Decimal or Float (if bytes that look like decimals, depending on decimal param)
        // - Original type (for non-bytes or non-numeric strings)
        // - Recursively applied to arrays/objects

        if td.is_bytes() {
            // Bytes can become integer, decimal, float, or stay bytes
            // We include both decimal and float since the parameter is runtime
            TypeDef::integer()
                .or_decimal()
                .or_float()
                .or_bytes()
                .infallible()
        } else if td.is_array() || td.is_object() {
            // Arrays and objects can contain promoted values
            td.or_integer().or_decimal().or_float()
        } else {
            // Other types pass through unchanged
            td
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::value;
    use crate::value::Kind;
    use crate::value::kind::{Collection, Field, Index};
    use ordered_float::NotNan;
    use rust_decimal::dec;
    use std::collections::BTreeMap;

    test_function![
        parse_numeric => ParseNumeric;

        integer_string {
            args: func_args![value: value!("123")],
            want: Ok(value!(123)),
            tdef: TypeDef::integer().or_decimal().or_float().or_bytes().infallible(),
        }

        decimal_string {
            args: func_args![value: value!("123.456")],
            want: Ok(Value::Decimal(dec!(123.456))),
            tdef: TypeDef::integer().or_decimal().or_float().or_bytes().infallible(),
        }

        decimal_string_explicit_true {
            args: func_args![value: value!("123.456"), decimal: true],
            want: Ok(Value::Decimal(dec!(123.456))),
            tdef: TypeDef::integer().or_decimal().or_float().or_bytes().infallible(),
        }

        float_string_decimal_false {
            args: func_args![value: value!("123.456"), decimal: false],
            want: Ok(Value::Float(NotNan::new(123.456).unwrap())),
            tdef: TypeDef::integer().or_decimal().or_float().or_bytes().infallible(),
        }

        non_numeric_string {
            args: func_args![value: value!("hello")],
            want: Ok(value!("hello")),
            tdef: TypeDef::integer().or_decimal().or_float().or_bytes().infallible(),
        }

        integer_passthrough {
            args: func_args![value: value!(42)],
            want: Ok(value!(42)),
            tdef: TypeDef::integer().infallible(),
        }

        array_with_mixed {
            args: func_args![value: value!(["123", "45.67", "hello"])],
            want: Ok(Value::Array(vec![
                Value::Integer(123),
                Value::Decimal(dec!(45.67)),
                value!("hello"),
            ])),
            // Type def is more specific with known indices
            tdef: TypeDef::array(
                Collection::<Index>::empty()
                    .with_known(Index::from(0), Kind::bytes())
                    .with_known(Index::from(1), Kind::bytes())
                    .with_known(Index::from(2), Kind::bytes())
            ).or_integer().or_decimal().or_float(),
        }

        array_with_mixed_float {
            args: func_args![value: value!(["123", "45.67", "hello"]), decimal: false],
            want: Ok(Value::Array(vec![
                Value::Integer(123),
                Value::Float(NotNan::new(45.67).unwrap()),
                value!("hello"),
            ])),
            // Type def is more specific with known indices
            tdef: TypeDef::array(
                Collection::<Index>::empty()
                    .with_known(Index::from(0), Kind::bytes())
                    .with_known(Index::from(1), Kind::bytes())
                    .with_known(Index::from(2), Kind::bytes())
            ).or_integer().or_decimal().or_float(),
        }

        object_with_mixed {
            args: func_args![value: {
                let mut map = BTreeMap::new();
                map.insert("amount".into(), value!("99.95"));
                map.insert("count".into(), value!("42"));
                Value::Object(map)
            }],
            want: Ok({
                let mut map = BTreeMap::new();
                map.insert("amount".into(), Value::Decimal(dec!(99.95)));
                map.insert("count".into(), Value::Integer(42));
                Value::Object(map)
            }),
            // Type def is more specific with known fields
            tdef: TypeDef::object(
                Collection::<Field>::empty()
                    .with_known(Field::from("amount"), Kind::bytes())
                    .with_known(Field::from("count"), Kind::bytes())
            ).or_integer().or_decimal().or_float(),
        }
    ];
}
