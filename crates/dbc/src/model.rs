//! In-memory representation of a CAN database (`.dbc`) file.
//!
//! The model is editor-oriented: comments, value descriptions and attribute
//! values are attached directly to the objects they describe so the UI can
//! edit them in place and renames never leave dangling references. Constructs
//! we do not model explicitly (signal groups, relation attributes, …) are
//! preserved verbatim in [`Dbc::extra`] so a load/save round-trip never
//! discards data.

use serde::{Deserialize, Serialize};

/// Byte order of a signal on the wire.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ByteOrder {
    /// `@1` — Intel / little-endian.
    LittleEndian,
    /// `@0` — Motorola / big-endian.
    BigEndian,
}

impl ByteOrder {
    pub const ALL: [ByteOrder; 2] = [ByteOrder::LittleEndian, ByteOrder::BigEndian];

    pub fn label(self) -> &'static str {
        match self {
            ByteOrder::LittleEndian => "Little (Intel)",
            ByteOrder::BigEndian => "Big (Motorola)",
        }
    }
}

/// How the raw bits are interpreted. Floats come from `SIG_VALTYPE_`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ValueType {
    Unsigned,
    Signed,
    /// IEEE-754 single precision (32 bit signal).
    Float32,
    /// IEEE-754 double precision (64 bit signal).
    Float64,
}

impl ValueType {
    pub const ALL: [ValueType; 4] = [ValueType::Unsigned, ValueType::Signed, ValueType::Float32, ValueType::Float64];

    pub fn label(self) -> &'static str {
        match self {
            ValueType::Unsigned => "Unsigned",
            ValueType::Signed => "Signed",
            ValueType::Float32 => "Float32",
            ValueType::Float64 => "Float64",
        }
    }

    pub fn is_float(self) -> bool {
        matches!(self, ValueType::Float32 | ValueType::Float64)
    }
}

/// Multiplexing role of a signal within its message.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Multiplexer {
    /// Ordinary signal, always present.
    None,
    /// The multiplexor switch (`M`).
    Multiplexor,
    /// Multiplexed signal, present only when the switch equals this value (`m<n>`).
    Multiplexed(u64),
    /// Multiplexed by an outer switch and itself a switch (`m<n>M`, extended mux).
    MultiplexedMultiplexor(u64),
}

impl Multiplexer {
    pub fn is_switch(&self) -> bool {
        matches!(self, Multiplexer::Multiplexor | Multiplexer::MultiplexedMultiplexor(_))
    }

    pub fn mux_value(&self) -> Option<u64> {
        match self {
            Multiplexer::Multiplexed(v) | Multiplexer::MultiplexedMultiplexor(v) => Some(*v),
            _ => None,
        }
    }
}

/// Extended multiplexing (`SG_MUL_VAL_`): the switch signal and the inclusive
/// value ranges for which this signal is present.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExtMux {
    pub switch: String,
    pub ranges: Vec<(u64, u64)>,
}

/// An attribute value as stored on an object. Enum values are stored as the
/// index into the definition's variant list, as in the file format.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum AttrValue {
    Int(i64),
    Float(f64),
    Str(String),
}

impl AttrValue {
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            AttrValue::Int(v) => Some(*v as f64),
            AttrValue::Float(v) => Some(*v),
            AttrValue::Str(s) => s.parse().ok(),
        }
    }
}

/// Which kind of object an attribute definition applies to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum AttrObject {
    Network,
    Node,
    Message,
    Signal,
    EnvVar,
}

impl AttrObject {
    pub const ALL: [AttrObject; 5] = [AttrObject::Network, AttrObject::Node, AttrObject::Message, AttrObject::Signal, AttrObject::EnvVar];

    pub fn keyword(self) -> &'static str {
        match self {
            AttrObject::Network => "",
            AttrObject::Node => "BU_",
            AttrObject::Message => "BO_",
            AttrObject::Signal => "SG_",
            AttrObject::EnvVar => "EV_",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            AttrObject::Network => "Network",
            AttrObject::Node => "Node",
            AttrObject::Message => "Message",
            AttrObject::Signal => "Signal",
            AttrObject::EnvVar => "Env. variable",
        }
    }
}

/// Value domain of an attribute definition.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum AttrKind {
    Int { min: i64, max: i64 },
    Hex { min: i64, max: i64 },
    Float { min: f64, max: f64 },
    Str,
    Enum(Vec<String>),
}

impl AttrKind {
    pub fn label(&self) -> &'static str {
        match self {
            AttrKind::Int { .. } => "INT",
            AttrKind::Hex { .. } => "HEX",
            AttrKind::Float { .. } => "FLOAT",
            AttrKind::Str => "STRING",
            AttrKind::Enum(_) => "ENUM",
        }
    }

    /// Coerce a loosely typed literal into the representation this kind uses.
    pub fn normalize(&self, v: AttrValue) -> AttrValue {
        match (self, v) {
            (AttrKind::Float { .. }, AttrValue::Int(i)) => AttrValue::Float(i as f64),
            (AttrKind::Int { .. } | AttrKind::Hex { .. }, AttrValue::Float(f)) => AttrValue::Int(f as i64),
            (AttrKind::Enum(variants), AttrValue::Str(s)) => {
                variants.iter().position(|x| *x == s).map(|i| AttrValue::Int(i as i64)).unwrap_or(AttrValue::Str(s))
            }
            (AttrKind::Str, AttrValue::Int(i)) => AttrValue::Str(i.to_string()),
            (_, v) => v,
        }
    }

    /// Human-readable rendering of a value of this kind (enum index -> name).
    pub fn display(&self, v: &AttrValue) -> String {
        match (self, v) {
            (AttrKind::Enum(variants), AttrValue::Int(i)) => variants.get(*i as usize).cloned().unwrap_or_else(|| i.to_string()),
            (_, AttrValue::Int(i)) => i.to_string(),
            (_, AttrValue::Float(f)) => f.to_string(),
            (_, AttrValue::Str(s)) => s.clone(),
        }
    }
}

/// `BA_DEF_` + `BA_DEF_DEF_`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AttributeDef {
    pub name: String,
    pub object: AttrObject,
    pub kind: AttrKind,
    pub default: Option<AttrValue>,
}

/// Attribute values set on one object (`BA_`), in file order.
pub type Attributes = Vec<(String, AttrValue)>;

/// A single signal (`SG_`) inside a message.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Signal {
    pub name: String,
    pub multiplexer: Multiplexer,
    pub start_bit: u64,
    pub size: u64,
    pub byte_order: ByteOrder,
    pub value_type: ValueType,
    pub factor: f64,
    pub offset: f64,
    pub min: f64,
    pub max: f64,
    pub unit: String,
    pub receivers: Vec<String>,
    pub comment: Option<String>,
    /// `VAL_` value descriptions: (raw value, label).
    pub value_descriptions: Vec<(i64, String)>,
    #[serde(default)]
    pub ext_mux: Option<ExtMux>,
    #[serde(default)]
    pub attributes: Attributes,
}

impl Signal {
    pub fn new(name: impl Into<String>, start_bit: u64) -> Self {
        Signal {
            name: name.into(),
            multiplexer: Multiplexer::None,
            start_bit,
            size: 8,
            byte_order: ByteOrder::LittleEndian,
            value_type: ValueType::Unsigned,
            factor: 1.0,
            offset: 0.0,
            min: 0.0,
            max: 0.0,
            unit: String::new(),
            receivers: vec![VECTOR_XXX.to_string()],
            comment: None,
            value_descriptions: Vec::new(),
            ext_mux: None,
            attributes: Vec::new(),
        }
    }

    /// Label for a raw value from the signal's value table, if any.
    pub fn label_for(&self, raw: i64) -> Option<&str> {
        self.value_descriptions.iter().find(|(v, _)| *v == raw).map(|(_, l)| l.as_str())
    }
}

/// A CAN message (`BO_`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Message {
    /// Raw message id. Bit 31 set marks an extended (29-bit) frame.
    pub id: u32,
    pub name: String,
    /// Payload length in bytes (up to 64 for CAN-FD).
    pub size: u64,
    pub transmitter: String,
    pub signals: Vec<Signal>,
    pub comment: Option<String>,
    #[serde(default)]
    pub attributes: Attributes,
}

/// The extended-frame flag stored in bit 31 of a DBC message id.
pub const EXTENDED_FLAG: u32 = 0x8000_0000;
/// Placeholder node name used by DBC tools for "no node".
pub const VECTOR_XXX: &str = "Vector__XXX";

impl Message {
    pub fn new(id: u32, name: impl Into<String>) -> Self {
        Message {
            id,
            name: name.into(),
            size: 8,
            transmitter: VECTOR_XXX.to_string(),
            signals: Vec::new(),
            comment: None,
            attributes: Vec::new(),
        }
    }

    /// True when the message uses a 29-bit extended identifier.
    pub fn is_extended(&self) -> bool {
        self.id & EXTENDED_FLAG != 0
    }

    /// The functional identifier with the extended flag masked off.
    pub fn raw_id(&self) -> u32 {
        self.id & !EXTENDED_FLAG
    }

    pub fn signal(&self, name: &str) -> Option<&Signal> {
        self.signals.iter().find(|s| s.name == name)
    }
}

/// A network node / ECU (`BU_`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Node {
    pub name: String,
    pub comment: Option<String>,
    #[serde(default)]
    pub attributes: Attributes,
}

impl Node {
    pub fn new(name: impl Into<String>) -> Self {
        Node { name: name.into(), comment: None, attributes: Vec::new() }
    }
}

/// A global value table (`VAL_TABLE_`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ValueTable {
    pub name: String,
    pub values: Vec<(i64, String)>,
}

/// A complete CAN database.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Dbc {
    pub version: String,
    /// `NS_` new-symbol entries, preserved verbatim.
    pub new_symbols: Vec<String>,
    pub nodes: Vec<Node>,
    pub value_tables: Vec<ValueTable>,
    pub messages: Vec<Message>,
    /// Network-level comment (`CM_ "...";`).
    pub comment: Option<String>,
    #[serde(default)]
    pub attribute_defs: Vec<AttributeDef>,
    /// Network-level attribute values.
    #[serde(default)]
    pub attributes: Attributes,
    /// Lines we do not model, preserved verbatim and re-emitted on save.
    pub extra: Vec<String>,
}

impl Dbc {
    pub fn find_message(&self, id: u32) -> Option<&Message> {
        self.messages.iter().find(|m| m.id == id)
    }

    pub fn message_mut(&mut self, idx: usize) -> Option<&mut Message> {
        self.messages.get_mut(idx)
    }

    /// Total number of signals across all messages.
    pub fn signal_count(&self) -> usize {
        self.messages.iter().map(|m| m.signals.len()).sum()
    }

    pub fn attribute_def(&self, name: &str) -> Option<&AttributeDef> {
        self.attribute_defs.iter().find(|d| d.name == name)
    }

    /// Effective attribute value: the object's own value, else the default.
    pub fn attr<'a>(&'a self, set: &'a Attributes, name: &str) -> Option<&'a AttrValue> {
        set.iter().find(|(n, _)| n == name).map(|(_, v)| v).or_else(|| self.attribute_def(name)?.default.as_ref())
    }

    /// Effective attribute value rendered for display (enum names resolved).
    pub fn attr_display(&self, set: &Attributes, name: &str) -> Option<String> {
        let v = self.attr(set, name)?;
        Some(match self.attribute_def(name) {
            Some(d) => d.kind.display(v),
            None => AttrKind::Str.display(v),
        })
    }

    /// Nominal cycle time of a message in milliseconds (`GenMsgCycleTime`).
    pub fn cycle_time_ms(&self, msg: &Message) -> Option<f64> {
        self.attr(&msg.attributes, "GenMsgCycleTime").and_then(AttrValue::as_f64).filter(|v| *v > 0.0)
    }

    /// True when the message is declared as CAN-FD (`VFrameFormat`).
    pub fn is_fd(&self, msg: &Message) -> bool {
        self.attr_display(&msg.attributes, "VFrameFormat").is_some_and(|f| f.contains("FD")) || msg.size > 8
    }

    /// Rename a node everywhere it is referenced.
    pub fn rename_node(&mut self, idx: usize, new: &str) {
        let old = std::mem::replace(&mut self.nodes[idx].name, new.to_string());
        for m in &mut self.messages {
            if m.transmitter == old {
                m.transmitter = new.to_string();
            }
            for s in &mut m.signals {
                for r in &mut s.receivers {
                    if *r == old {
                        *r = new.to_string();
                    }
                }
            }
        }
        for line in &mut self.extra {
            if line.starts_with("BO_TX_BU_") || line.starts_with("BU_") {
                *line = replace_word(line, &old, new);
            }
        }
    }

    /// Change a message id, keeping verbatim references in [`Dbc::extra`] in sync.
    pub fn set_message_id(&mut self, idx: usize, new_id: u32) {
        let old = std::mem::replace(&mut self.messages[idx].id, new_id);
        if old == new_id {
            return;
        }
        let (old, new) = (old.to_string(), new_id.to_string());
        for line in &mut self.extra {
            for kw in ["BO_TX_BU_ ", "SIG_GROUP_ ", "BA_REL_ ", "BU_BO_REL_ ", "BU_SG_REL_ "] {
                if line.starts_with(kw) {
                    *line = replace_word(line, &old, &new);
                }
            }
        }
    }

    /// Rename a signal, keeping extended-mux switches and signal groups in sync.
    pub fn rename_signal(&mut self, msg_idx: usize, sig_idx: usize, new: &str) {
        let msg = &mut self.messages[msg_idx];
        let old = std::mem::replace(&mut msg.signals[sig_idx].name, new.to_string());
        for s in &mut msg.signals {
            if let Some(em) = &mut s.ext_mux
                && em.switch == old
            {
                em.switch = new.to_string();
            }
        }
        let id = msg.id.to_string();
        for line in &mut self.extra {
            let mut toks = line.split_whitespace();
            let kw = toks.next();
            if matches!(kw, Some("SIG_GROUP_" | "BA_REL_" | "BU_SG_REL_")) && line.split_whitespace().any(|t| t == id) {
                *line = replace_word(line, &old, new);
            }
        }
    }
}

/// Replace whole-word occurrences of `old` in `line` (word chars: alnum, `_`).
fn replace_word(line: &str, old: &str, new: &str) -> String {
    let is_word = |c: char| c.is_ascii_alphanumeric() || c == '_';
    let mut out = String::with_capacity(line.len());
    let mut rest = line;
    while let Some(pos) = rest.find(old) {
        let before = rest[..pos].chars().next_back();
        let after = rest[pos + old.len()..].chars().next();
        out.push_str(&rest[..pos]);
        if before.is_none_or(|c| !is_word(c)) && after.is_none_or(|c| !is_word(c)) {
            out.push_str(new);
        } else {
            out.push_str(old);
        }
        rest = &rest[pos + old.len()..];
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::replace_word;

    #[test]
    fn replace_word_respects_boundaries() {
        assert_eq!(replace_word("SIG_GROUP_ 10 G 1 : A AB A;", "A", "Z"), "SIG_GROUP_ 10 G 1 : Z AB Z;");
        assert_eq!(replace_word("BO_TX_BU_ 100 : X,Y;", "100", "7"), "BO_TX_BU_ 7 : X,Y;");
    }
}
