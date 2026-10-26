//! A server's message, read into only what the check looks at. A line can be [`super::MAX_LINE`] long: read into a
//! `serde_json::Value`, a 1 MB list of 19,000 small tools took the check from 12 MB to 39 MB (measured, debug build),
//! and with several checks at once that adds up. Here every part the check does not look at is skipped as it is
//! read, long texts are not kept, and the tools are judged one by one as they go by, so a message costs a few small
//! allocations whatever its size.
//!
//! Where an object has the same key twice, the last wins, as in JavaScript (Claude Code). A part of the wrong kind is
//! read as "not that kind", never as an error: a line is refused only when it is not JSON.
use serde::Deserialize;
use serde::de::{Deserializer, IgnoredAny, MapAccess, SeqAccess, Visitor};
use serde_json::Value;
use std::fmt;
use std::marker::PhantomData;

/// Texts kept for comparing (a method, a protocol version, a schema's type), at most this many bytes; a longer one is
/// still known to be text.
const SHORT: usize = 64;
/// How much of an answer's error message is kept, in bytes, for the reason shown.
const MESSAGE: usize = 1024;

/// One message from the server: a request or notification (`method`), or an answer (`result` or `error`).
#[derive(Debug, Default)]
pub(super) struct Message {
    /// A number or a text, as it is sent back; `None` when absent or of another kind.
    pub id: Option<Value>,
    /// Present when the method is text; the text itself only when short.
    pub method: Option<Option<String>>,
    /// Present, whatever its kind (one that is no object reads as an empty one).
    pub result: Option<Body>,
    /// Present, whatever its kind.
    pub error: Option<ErrorInfo>,
}

/// The parts of a `result` the check looks at: those of `server/discover`, `initialize` and `tools/list` answers.
#[derive(Debug, Default)]
pub(super) struct Body {
    /// `supportedVersions` is a list that holds the newer protocol.
    pub modern: bool,
    /// `protocolVersion`, when it is text (the text itself only when short).
    pub protocol_version: Option<Option<String>>,
    /// `capabilities`, when it is an object.
    pub capabilities: Option<Capabilities>,
    /// `serverInfo`, when it is an object.
    pub server_info: Option<ServerInfo>,
    pub tools: Tools,
}

#[derive(Debug, Default, Clone, Copy)]
pub(super) struct Capabilities {
    /// It has `tools`, of any kind (`null` too).
    pub tools: bool,
}

#[derive(Debug, Default, Clone, Copy)]
pub(super) struct ServerInfo {
    pub name_is_text: bool,
    pub version_is_text: bool,
}

/// What `tools` is.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(super) enum Tools {
    #[default]
    Absent,
    NotAList,
    List {
        count: usize,
        /// Every tool is described the way Claude Code accepts.
        good: bool,
    },
}

/// An answer's `error`, whatever its kind: its message and code when it has them.
#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct ErrorInfo {
    pub message: Option<String>,
    pub code: Option<i64>,
}

impl Message {
    /// A line that is a JSON object, or `None`.
    pub(super) fn parse(line: &[u8]) -> Option<Self> {
        match serde_json::from_slice::<R<Object<Self>>>(line) {
            Ok(R(Object::Is(message))) => Some(message),
            _ => None,
        }
    }

    /// `value` read as a message would be, for tests.
    #[cfg(test)]
    pub(super) fn from_value(value: Value) -> Option<Self> {
        Self::parse(value.to_string().as_bytes())
    }
}

/// A value that is not a list or an object.
enum Scalar<'a> {
    Null,
    Bool,
    Number(Option<Value>),
    Text(&'a str),
}

/// How a type is read from any JSON value: a list's and an object's contents are skipped unless it reads them.
trait Read: Sized {
    fn scalar(value: Scalar<'_>) -> Self;
    fn object<'de, A: MapAccess<'de>>(mut map: A) -> Result<Self, A::Error> {
        while map.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
        Ok(Self::other_container(true))
    }
    fn list<'de, A: SeqAccess<'de>>(mut seq: A) -> Result<Self, A::Error> {
        while seq.next_element::<IgnoredAny>()?.is_some() {}
        Ok(Self::other_container(false))
    }
    /// A list or object (`object`) whose contents were skipped.
    fn other_container(object: bool) -> Self;
}

/// Reads a [`Read`] type.
struct R<T>(T);

impl<'de, T: Read> Deserialize<'de> for R<T> {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V<T>(PhantomData<T>);
        impl<'de, T: Read> Visitor<'de> for V<T> {
            type Value = R<T>;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("any JSON value")
            }
            fn visit_map<A: MapAccess<'de>>(self, map: A) -> Result<R<T>, A::Error> {
                T::object(map).map(R)
            }
            fn visit_seq<A: SeqAccess<'de>>(self, seq: A) -> Result<R<T>, A::Error> {
                T::list(seq).map(R)
            }
            fn visit_unit<E>(self) -> Result<R<T>, E> {
                Ok(R(T::scalar(Scalar::Null)))
            }
            fn visit_bool<E>(self, _: bool) -> Result<R<T>, E> {
                Ok(R(T::scalar(Scalar::Bool)))
            }
            fn visit_i64<E>(self, n: i64) -> Result<R<T>, E> {
                Ok(R(T::scalar(Scalar::Number(Some(n.into())))))
            }
            fn visit_u64<E>(self, n: u64) -> Result<R<T>, E> {
                Ok(R(T::scalar(Scalar::Number(Some(n.into())))))
            }
            fn visit_f64<E>(self, n: f64) -> Result<R<T>, E> {
                let n = serde_json::Number::from_f64(n).map(Value::Number);
                Ok(R(T::scalar(Scalar::Number(n))))
            }
            fn visit_str<E>(self, s: &str) -> Result<R<T>, E> {
                Ok(R(T::scalar(Scalar::Text(s))))
            }
        }
        d.deserialize_any(V(PhantomData))
    }
}

/// The next value of an object, read as `T`.
fn next<'de, T: Read, A: MapAccess<'de>>(map: &mut A) -> Result<T, A::Error> {
    Ok(map.next_value::<R<T>>()?.0)
}

/// A value's kind, with a text kept when short and a number kept.
#[derive(Debug, Clone, PartialEq)]
enum Shape {
    Null,
    Bool,
    Number(Option<Value>),
    Text(Option<String>),
    List,
    Object,
}

impl Read for Shape {
    fn scalar(value: Scalar<'_>) -> Self {
        match value {
            Scalar::Null => Self::Null,
            Scalar::Bool => Self::Bool,
            Scalar::Number(n) => Self::Number(n),
            Scalar::Text(s) => Self::Text((s.len() <= SHORT).then(|| s.to_owned())),
        }
    }
    fn other_container(object: bool) -> Self {
        if object { Self::Object } else { Self::List }
    }
}

impl Shape {
    /// `Some` with the text (when short) for a text.
    fn text(self) -> Option<Option<String>> {
        match self {
            Self::Text(text) => Some(text),
            _ => None,
        }
    }
}

/// An id: a number, or a text kept whole (it is sent back as it came).
struct Id(Option<Value>);

impl Read for Id {
    fn scalar(value: Scalar<'_>) -> Self {
        Self(match value {
            Scalar::Number(n) => n,
            Scalar::Text(s) => Some(Value::String(s.to_owned())),
            Scalar::Null | Scalar::Bool => None,
        })
    }
    fn other_container(_: bool) -> Self {
        Self(None)
    }
}

/// A text, its first [`MESSAGE`] bytes kept.
struct LongText(Option<String>);

impl Read for LongText {
    fn scalar(value: Scalar<'_>) -> Self {
        Self(match value {
            Scalar::Text(s) => {
                let mut end = s.len().min(MESSAGE);
                while !s.is_char_boundary(end) {
                    end -= 1;
                }
                Some(s[..end].to_owned())
            }
            _ => None,
        })
    }
    fn other_container(_: bool) -> Self {
        Self(None)
    }
}

/// A list judged by its items (each read as a [`Shape`]), or `null`, or anything else.
enum Items {
    Null,
    List { all_text: bool, has_modern: bool },
    Other,
}

impl Read for Items {
    fn scalar(value: Scalar<'_>) -> Self {
        match value {
            Scalar::Null => Self::Null,
            _ => Self::Other,
        }
    }
    fn list<'de, A: SeqAccess<'de>>(mut seq: A) -> Result<Self, A::Error> {
        let (mut all_text, mut has_modern) = (true, false);
        while let Some(R(item)) = seq.next_element::<R<Shape>>()? {
            match item {
                Shape::Text(text) => has_modern |= text.as_deref() == Some(super::MODERN),
                _ => all_text = false,
            }
        }
        Ok(Self::List {
            all_text,
            has_modern,
        })
    }
    fn other_container(_: bool) -> Self {
        Self::Other
    }
}

/// An object's fields, read one key at a time; the value of a key it does not look at must be skipped.
trait Fields: Default {
    fn field<'de, A: MapAccess<'de>>(&mut self, key: &str, map: &mut A) -> Result<(), A::Error>;
}

/// An object read as `T`, or a value of another kind, skipped.
enum Object<T> {
    Is(T),
    Not,
}

impl<T> Object<T> {
    fn get(self) -> Option<T> {
        match self {
            Self::Is(t) => Some(t),
            Self::Not => None,
        }
    }
}

impl<T: Fields> Read for Object<T> {
    fn scalar(_: Scalar<'_>) -> Self {
        Self::Not
    }
    fn object<'de, A: MapAccess<'de>>(mut map: A) -> Result<Self, A::Error> {
        let mut fields = T::default();
        // A key is kept only when short: every key looked at is.
        while let Some(R(key)) = map.next_key::<R<Shape>>()? {
            match key {
                Shape::Text(Some(key)) => fields.field(&key, &mut map)?,
                _ => {
                    map.next_value::<IgnoredAny>()?;
                }
            }
        }
        Ok(Self::Is(fields))
    }
    fn other_container(_: bool) -> Self {
        Self::Not
    }
}

impl Fields for Message {
    fn field<'de, A: MapAccess<'de>>(&mut self, key: &str, map: &mut A) -> Result<(), A::Error> {
        match key {
            "id" => self.id = next::<Id, _>(map)?.0,
            "method" => self.method = next::<Shape, _>(map)?.text(),
            "result" => {
                self.result = Some(next::<Object<Body>, _>(map)?.get().unwrap_or_default());
            }
            "error" => {
                self.error = Some(next::<Object<ErrorInfo>, _>(map)?.get().unwrap_or_default());
            }
            _ => {
                map.next_value::<IgnoredAny>()?;
            }
        }
        Ok(())
    }
}

impl Fields for Body {
    fn field<'de, A: MapAccess<'de>>(&mut self, key: &str, map: &mut A) -> Result<(), A::Error> {
        match key {
            "supportedVersions" => {
                self.modern = matches!(
                    next::<Items, _>(map)?,
                    Items::List {
                        has_modern: true,
                        ..
                    }
                );
            }
            "protocolVersion" => self.protocol_version = next::<Shape, _>(map)?.text(),
            "capabilities" => self.capabilities = next::<Object<Capabilities>, _>(map)?.get(),
            "serverInfo" => self.server_info = next::<Object<ServerInfo>, _>(map)?.get(),
            "tools" => self.tools = next::<ToolList, _>(map)?.0,
            _ => {
                map.next_value::<IgnoredAny>()?;
            }
        }
        Ok(())
    }
}

impl Fields for Capabilities {
    fn field<'de, A: MapAccess<'de>>(&mut self, key: &str, map: &mut A) -> Result<(), A::Error> {
        map.next_value::<IgnoredAny>()?;
        self.tools |= key == "tools";
        Ok(())
    }
}

impl Fields for ServerInfo {
    fn field<'de, A: MapAccess<'de>>(&mut self, key: &str, map: &mut A) -> Result<(), A::Error> {
        let text = next::<Shape, _>(map)?.text().is_some();
        match key {
            "name" => self.name_is_text = text,
            "version" => self.version_is_text = text,
            _ => {}
        }
        Ok(())
    }
}

impl Fields for ErrorInfo {
    fn field<'de, A: MapAccess<'de>>(&mut self, key: &str, map: &mut A) -> Result<(), A::Error> {
        match key {
            "message" => self.message = next::<LongText, _>(map)?.0,
            "code" => {
                self.code = match next::<Shape, _>(map)? {
                    Shape::Number(Some(n)) => n.as_i64(),
                    _ => None,
                };
            }
            _ => {
                map.next_value::<IgnoredAny>()?;
            }
        }
        Ok(())
    }
}

/// `tools`, each item judged as it is read and then dropped.
struct ToolList(Tools);

impl Read for ToolList {
    fn scalar(_: Scalar<'_>) -> Self {
        Self(Tools::NotAList)
    }
    fn list<'de, A: SeqAccess<'de>>(mut seq: A) -> Result<Self, A::Error> {
        let (mut count, mut good) = (0, true);
        while let Some(R(tool)) = seq.next_element::<R<Object<Tool>>>()? {
            count += 1;
            good &= tool.get().is_some_and(|t| t.good());
        }
        Ok(Self(Tools::List { count, good }))
    }
    fn other_container(_: bool) -> Self {
        Self(Tools::NotAList)
    }
}

/// One tool, as far as Claude Code checks it. A field that is `None` was absent.
#[derive(Default)]
struct Tool {
    name: Option<Shape>,
    title: Option<Shape>,
    description: Option<Shape>,
    /// Whether the schema is one Claude Code accepts.
    input_schema: Option<bool>,
    output_schema: Option<bool>,
    annotations: Option<Shape>,
}

impl Tool {
    fn good(&self) -> bool {
        let text_or_absent =
            |v: &Option<Shape>| matches!(v, None | Some(Shape::Null | Shape::Text(_)));
        matches!(self.name, Some(Shape::Text(_)))
            && text_or_absent(&self.title)
            && text_or_absent(&self.description)
            && self.input_schema == Some(true)
            && self.output_schema != Some(false)
            && matches!(self.annotations, None | Some(Shape::Null | Shape::Object))
    }
}

impl Fields for Tool {
    fn field<'de, A: MapAccess<'de>>(&mut self, key: &str, map: &mut A) -> Result<(), A::Error> {
        let schema = |map: &mut A| -> Result<Option<bool>, A::Error> {
            Ok(Some(
                next::<Object<Schema>, _>(map)?
                    .get()
                    .is_some_and(|s| s.good()),
            ))
        };
        match key {
            "name" => self.name = Some(next(map)?),
            "title" => self.title = Some(next(map)?),
            "description" => self.description = Some(next(map)?),
            "inputSchema" => self.input_schema = schema(map)?,
            "outputSchema" => self.output_schema = schema(map)?,
            "annotations" => self.annotations = Some(next(map)?),
            _ => {
                map.next_value::<IgnoredAny>()?;
            }
        }
        Ok(())
    }
}

/// A tool's schema, as far as Claude Code checks it.
struct Schema {
    /// `type` is `"object"`.
    object_type: bool,
    /// `properties` is absent, `null` or an object.
    properties: bool,
    /// `required` is absent, `null` or a list of texts.
    required: bool,
}

impl Default for Schema {
    fn default() -> Self {
        Self {
            object_type: false,
            properties: true,
            required: true,
        }
    }
}

impl Schema {
    fn good(&self) -> bool {
        self.object_type && self.properties && self.required
    }
}

impl Fields for Schema {
    fn field<'de, A: MapAccess<'de>>(&mut self, key: &str, map: &mut A) -> Result<(), A::Error> {
        match key {
            "type" => {
                self.object_type = next::<Shape, _>(map)?.text() == Some(Some("object".to_owned()));
            }
            "properties" => {
                self.properties = matches!(next::<Shape, _>(map)?, Shape::Null | Shape::Object);
            }
            "required" => {
                self.required = matches!(
                    next::<Items, _>(map)?,
                    Items::Null | Items::List { all_text: true, .. }
                );
            }
            _ => {
                map.next_value::<IgnoredAny>()?;
            }
        }
        Ok(())
    }
}
