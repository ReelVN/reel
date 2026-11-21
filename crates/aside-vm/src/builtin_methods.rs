//! One declaration set for runtime collection methods and source tooling.
use crate::{ObjectKind, executor::FaultKind};

/// An actual bound-method parameter. Type and default text use Aside syntax;
/// the compiler projects them through its ordinary parser, independently of VM IO.
#[derive(Clone, Copy, Debug)]
pub struct BuiltinParameter {
    pub name: &'static str,
    pub ty: &'static str,
    pub default: Option<&'static str>,
}

/// A runtime method's source contract. `Self` in the result denotes the
/// receiver's collection kind, preserving tuple kind without promising its size.
#[derive(Clone, Copy, Debug)]
pub struct BuiltinDeclaration {
    pub name: &'static str,
    pub receivers: &'static [ObjectKind],
    pub parameters: &'static [BuiltinParameter],
    pub rest: Option<BuiltinParameter>,
    pub result: &'static str,
    pub documentation: &'static str,
    pub mutates: bool,
}

macro_rules! methods {
    ($($variant:ident: $name:literal [$($receiver:ident),+] ($($parameter:literal : $ty:literal = $default:expr),*), $rest:expr => $result:literal, $mutates:literal, $documentation:literal;)+) => {
        /// Runtime bound methods. Variant order is the stored method identity;
        /// lookup, argument binding and editor contracts use the same declarations.
        #[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
        #[repr(i64)]
        pub enum BuiltinMethod { $($variant,)+ }

        impl BuiltinMethod {
            pub const ALL: &'static [Self] = &[$(Self::$variant,)+];

            pub fn declaration(self) -> &'static BuiltinDeclaration {
                match self {
                    $(Self::$variant => &BuiltinDeclaration {
                        name: $name,
                        receivers: &[$(ObjectKind::$receiver),+],
                        parameters: &[$(BuiltinParameter {name: $parameter, ty: $ty, default: $default}),*],
                        rest: $rest,
                        result: $result,
                        documentation: $documentation,
                        mutates: $mutates,
                    },)+
                }
            }

            pub fn lookup(kind: ObjectKind, name: &str) -> Option<Self> {
                match (kind, name) {
                    $(($(ObjectKind::$receiver)|+, $name) => Some(Self::$variant),)+
                    _ => None,
                }
            }
        }
    };
}

methods! {
    Len: "len" [Array,Tuple,Table,Bytes] (), None => "int", false,
        "Return the number of sequence elements, table entries or immutable bytes.";
    Append: "append" [Array] (), Some(BuiltinParameter {name:"values",ty:"any",default:None}) => "array", true,
        "Append positional values in order and return this array. An empty call preserves it.";
    Pop: "pop" [Array] (), None => "any", true,
        "Remove and return the last value. An empty array raises OutOfBounds.";
    HasOwn: "has_own" [Table] ("key":"any"=None), None => "bool", false,
        "Return whether this table contains its own key. Reactive reads observe the matched field, or the collection when the key is missing.";
    Slice: "slice" [Array,Tuple] ("range":"Range"=None), None => "Self", false,
        "Copy the selected range into a new sequence of the same kind.";
    Insert: "insert" [Array] ("index":"int"=None,"value":"any"=None), None => "array", true,
        "Insert a value at an index, allowing the end position, and return this array.";
    Remove: "remove" [Array] ("index":"int"=None), None => "any", true,
        "Remove and return the value at an existing index.";
    Resize: "resize" [Array] ("length":"int"=None,"value":"any"=Some("null")), None => "array", true,
        "Resize to a nonnegative length, fill new elements with value, and return this array.";
    Reverse: "reverse" [Array] (), None => "array", true,
        "Reverse the elements and return this array.";
    Extend: "extend" [Array] ("items":"array | tuple"=None), None => "array", true,
        "Append an array or tuple's elements and return this array.";
    Clear: "clear" [Array,Table] (), None => "Self", true,
        "Remove all elements or entries and return this collection.";
    Top: "top" [Array] (), None => "any", false,
        "Return the last value without removing it. An empty array raises OutOfBounds.";
    Swap: "swap" [Array] ("a":"int"=None,"b":"int"=None), None => "array", true,
        "Exchange two existing elements and return this array.";
    Keys: "keys" [Table] (), None => "array", false,
        "Copy table keys into an array in entry order.";
    Values: "values" [Table] (), None => "array", false,
        "Copy table values into an array in entry order.";
    Entries: "entries" [Table] (), None => "array", false,
        "Copy table entries into an array of key/value tuples in entry order.";
    ByteLen: "byte_len" [String] (), None => "int", false,
        "Return the UTF-8 byte length.";
    ScalarCount: "scalar_count" [String] (), None => "int", false,
        "Count Unicode scalar values.";
    SliceBytes: "slice_bytes" [String] ("range":"Range"=None), None => "string", false,
        "Return the selected UTF-8 byte range. Endpoints must be scalar boundaries.";
    SliceScalars: "slice_scalars" [String] ("range":"Range"=None), None => "string", false,
        "Return the selected Unicode scalar range.";
    StartsWith: "starts_with" [String] ("prefix":"string"=None), None => "bool", false,
        "Return whether this string starts with prefix.";
    EndsWith: "ends_with" [String] ("suffix":"string"=None), None => "bool", false,
        "Return whether this string ends with suffix.";
    Join: "join" [String] ("items":"array | tuple"=None), None => "string", false,
        "Join an array or tuple of strings using this string as the separator.";
    AsciiLowercase: "to_ascii_lowercase" [String] (), None => "string", false,
        "Convert ASCII uppercase letters to lowercase, preserving other Unicode text.";
    AsciiUppercase: "to_ascii_uppercase" [String] (), None => "string", false,
        "Convert ASCII lowercase letters to uppercase, preserving other Unicode text.";
    Trim: "trim" [String] (), None => "string", false,
        "Remove leading and trailing Unicode whitespace.";
    TrimStart: "trim_start" [String] (), None => "string", false,
        "Remove leading Unicode whitespace.";
    TrimEnd: "trim_end" [String] (), None => "string", false,
        "Remove trailing Unicode whitespace.";
    Get: "get" [Table] ("key":"any"=None,"default":"any"=Some("null")), None => "any", false,
        "Return the stored value, or default when the key is absent. Reactive reads observe the matched field, or the collection when the key is missing.";
    Split: "split" [String] ("separator":"string"=None), None => "array", false,
        "Split at a string separator, preserving empty fields. An empty separator splits at Unicode scalar boundaries.";
    Sort: "sort" [Array] ("compare":"any"=Some("null")), None => "array", true,
        "Sort stably and return this array. A supplied callable returns a negative, zero or positive int and cannot wait; null uses the default order.";
    TableRemove: "remove" [Table] ("key":"any"=None), None => "any", true,
        "Remove and return an existing entry's value. A missing key raises MissingKey.";
    Find: "find" [Array,Tuple] ("value":"any"=None), None => "int | null", false,
        "Return the first index equal to value, or null when absent. Comparisons use ordinary Aside equality.";
    Contains: "contains" [Array,Tuple] ("value":"any"=None), None => "bool", false,
        "Return whether any element equals value using ordinary Aside equality.";
    GeneratorNext: "next" [Generator] (), None => "(bool, any)", true,
        "Resume this generator and return (done, value). A yielded null has done=false. Completion returns its final value once; later calls return (true, null). Escaped errors close the generator and retain their original source location. Reentry raises InvalidState.";
    GeneratorClose: "close" [Generator] (), None => "null", true,
        "Close this generator and synchronously run finally blocks entered before its last yield. Closing an unstarted or finished generator returns null. Cleanup may yield its execution budget and cannot wait externally.";
    FirstWhere: "first_where" [Array,Tuple] ("predicate":"fn(any) -> bool"=None), None => "any", false,
        "Return the first item accepted by the predicate, or null. Visit items in order and stop at the first match. The predicate cannot wait or change the source.";
    Any: "any" [Array,Tuple] ("predicate":"fn(any) -> bool"=None), None => "bool", false,
        "Return whether the predicate accepts any item. Visit items in order and stop at the first match. The predicate cannot wait or change the source.";
    GetOrInsertWith: "get_or_insert_with" [Table] ("key":"any"=None,"factory":"fn() -> any"=None), None => "any", true,
        "Return the existing value, including null, or insert the result of calling factory(). The factory cannot wait. An entry inserted during the factory takes precedence.";
    Replace: "replace" [String] ("pattern":"string"=None,"replacement":"string"=None), None => "string", false,
        "Replace all non-overlapping matches. An empty pattern inserts the replacement at each Unicode scalar boundary.";
    StripPrefix: "strip_prefix" [String] ("prefix":"string"=None), None => "string | null", false,
        "Remove the given prefix if it matches. Return null when absent and the original string for an empty prefix.";
    StripSuffix: "strip_suffix" [String] ("suffix":"string"=None), None => "string | null", false,
        "Remove the given suffix if it matches. Return null when absent and the original string for an empty suffix.";
    IndexWhere: "index_where" [Array,Tuple] ("predicate":"fn(any) -> bool"=None), None => "int | null", false,
        "Return the zero-based index of the first item accepted by the predicate, or null. The predicate cannot wait or change the source.";
    All: "all" [Array,Tuple] ("predicate":"fn(any) -> bool"=None), None => "bool", false,
        "Return whether the predicate accepts every item. Stop at the first rejection. An empty sequence returns true. The predicate cannot wait or change the source.";
    Count: "count" [Array,Tuple] ("predicate":"fn(any) -> bool"=None), None => "int", false,
        "Count items accepted by the predicate. Visit items in source order. The predicate cannot wait or change the source.";
    Map: "map" [Array,Tuple] ("transform":"fn(any) -> any"=None), None => "array", false,
        "Collect the transform results into a new array in source order. The transform cannot wait or change the source.";
    Filter: "filter" [Array,Tuple] ("predicate":"fn(any) -> bool"=None), None => "array", false,
        "Collect the original items accepted by the predicate into a new array in source order. The predicate cannot wait or change the source.";
    StringContains: "contains" [String] ("pattern":"string"=None), None => "bool", false,
        "Return whether the string contains a literal substring. The empty pattern always matches.";
    StringFind: "find" [String] ("pattern":"string"=None), None => "int | null", false,
        "Return the first match's zero-based Unicode scalar index, or null. The empty pattern matches at zero. Neither string is normalized.";
}

impl BuiltinMethod {
    pub(crate) fn decode(id: i64) -> Result<Self, FaultKind> {
        usize::try_from(id)
            .ok()
            .and_then(|index| Self::ALL.get(index))
            .copied()
            .ok_or(FaultKind::Type)
    }

    pub(crate) fn parameters(self) -> (&'static [BuiltinParameter], usize) {
        let parameters = self.declaration().parameters;
        let required = parameters
            .iter()
            .take_while(|p| p.default.is_none())
            .count();
        (parameters, required)
    }

    pub(crate) fn mutates(self) -> bool {
        self.declaration().mutates
    }
}
