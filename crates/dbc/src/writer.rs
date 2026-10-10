//! Serialise a [`Dbc`] back into canonical DBC text.

use super::model::*;
use std::fmt::Write as _;

/// Render a [`Dbc`] to a `.dbc` document string.
pub fn write(dbc: &Dbc) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "VERSION \"{}\"\n", escape(&dbc.version));

    out.push_str("NS_ :\n");
    let symbols: Vec<&str> =
        if dbc.new_symbols.is_empty() { DEFAULT_NS.to_vec() } else { dbc.new_symbols.iter().map(String::as_str).collect() };
    for sym in symbols {
        let _ = writeln!(out, "\t{sym}");
    }
    out.push_str("\nBS_:\n\nBU_:");
    for n in &dbc.nodes {
        let _ = write!(out, " {}", n.name);
    }
    out.push('\n');

    for vt in &dbc.value_tables {
        let _ = write!(out, "VAL_TABLE_ {}", vt.name);
        write_value_pairs(&mut out, &vt.values);
        out.push_str(" ;\n");
    }
    out.push('\n');

    for m in &dbc.messages {
        let _ = writeln!(out, "BO_ {} {}: {} {}", m.id, m.name, m.size, m.transmitter);
        for s in &m.signals {
            write_signal(&mut out, s);
        }
        out.push('\n');
    }

    // Comments: network, nodes, messages, signals.
    if let Some(c) = &dbc.comment {
        let _ = writeln!(out, "CM_ \"{}\";", escape(c));
    }
    for n in &dbc.nodes {
        if let Some(c) = &n.comment {
            let _ = writeln!(out, "CM_ BU_ {} \"{}\";", n.name, escape(c));
        }
    }
    for m in &dbc.messages {
        if let Some(c) = &m.comment {
            let _ = writeln!(out, "CM_ BO_ {} \"{}\";", m.id, escape(c));
        }
    }
    for m in &dbc.messages {
        for s in &m.signals {
            if let Some(c) = &s.comment {
                let _ = writeln!(out, "CM_ SG_ {} {} \"{}\";", m.id, s.name, escape(c));
            }
        }
    }

    // Attribute definitions, defaults and values.
    for d in &dbc.attribute_defs {
        write_attr_def(&mut out, d);
    }
    for d in &dbc.attribute_defs {
        if let Some(v) = &d.default {
            let _ = writeln!(out, "BA_DEF_DEF_ \"{}\" {};", d.name, fmt_attr(v));
        }
    }
    for (name, v) in &dbc.attributes {
        let _ = writeln!(out, "BA_ \"{name}\" {};", fmt_attr(v));
    }
    for n in &dbc.nodes {
        for (name, v) in &n.attributes {
            let _ = writeln!(out, "BA_ \"{name}\" BU_ {} {};", n.name, fmt_attr(v));
        }
    }
    for m in &dbc.messages {
        for (name, v) in &m.attributes {
            let _ = writeln!(out, "BA_ \"{name}\" BO_ {} {};", m.id, fmt_attr(v));
        }
    }
    for m in &dbc.messages {
        for s in &m.signals {
            for (name, v) in &s.attributes {
                let _ = writeln!(out, "BA_ \"{name}\" SG_ {} {} {};", m.id, s.name, fmt_attr(v));
            }
        }
    }

    // Passthrough lines we do not model (signal groups, relations, …).
    for line in &dbc.extra {
        out.push_str(line);
        out.push('\n');
    }

    for m in &dbc.messages {
        for s in &m.signals {
            if !s.value_descriptions.is_empty() {
                let _ = write!(out, "VAL_ {} {}", m.id, s.name);
                write_value_pairs(&mut out, &s.value_descriptions);
                out.push_str(" ;\n");
            }
        }
    }
    for m in &dbc.messages {
        for s in &m.signals {
            let kind = match s.value_type {
                ValueType::Float32 => 1,
                ValueType::Float64 => 2,
                _ => continue,
            };
            let _ = writeln!(out, "SIG_VALTYPE_ {} {} : {kind};", m.id, s.name);
        }
    }
    for m in &dbc.messages {
        for s in &m.signals {
            if let Some(e) = &s.ext_mux {
                let ranges: Vec<String> = e.ranges.iter().map(|(lo, hi)| format!("{lo}-{hi}")).collect();
                let _ = writeln!(out, "SG_MUL_VAL_ {} {} {} {};", m.id, s.name, e.switch, ranges.join(", "));
            }
        }
    }
    out
}

fn write_signal(out: &mut String, s: &Signal) {
    let _ = write!(out, " SG_ {}", s.name);
    match &s.multiplexer {
        Multiplexer::None => {}
        Multiplexer::Multiplexor => out.push_str(" M"),
        Multiplexer::Multiplexed(n) => {
            let _ = write!(out, " m{n}");
        }
        Multiplexer::MultiplexedMultiplexor(n) => {
            let _ = write!(out, " m{n}M");
        }
    }
    let order = match s.byte_order {
        ByteOrder::LittleEndian => '1',
        ByteOrder::BigEndian => '0',
    };
    let sign = match s.value_type {
        ValueType::Unsigned => '+',
        _ => '-',
    };
    let _ = write!(
        out,
        " : {}|{}@{}{} ({},{}) [{}|{}] \"{}\"",
        s.start_bit,
        s.size,
        order,
        sign,
        fmt_f64(s.factor),
        fmt_f64(s.offset),
        fmt_f64(s.min),
        fmt_f64(s.max),
        escape(&s.unit),
    );
    if s.receivers.is_empty() {
        let _ = writeln!(out, " {VECTOR_XXX}");
    } else {
        let _ = writeln!(out, " {}", s.receivers.join(","));
    }
}

fn write_attr_def(out: &mut String, d: &AttributeDef) {
    let obj = d.object.keyword();
    let _ = write!(out, "BA_DEF_ {obj}{}\"{}\" ", if obj.is_empty() { "" } else { " " }, d.name);
    match &d.kind {
        AttrKind::Int { min, max } => {
            let _ = write!(out, "INT {min} {max}");
        }
        AttrKind::Hex { min, max } => {
            let _ = write!(out, "HEX {min} {max}");
        }
        AttrKind::Float { min, max } => {
            let _ = write!(out, "FLOAT {} {}", fmt_f64(*min), fmt_f64(*max));
        }
        AttrKind::Str => out.push_str("STRING "),
        AttrKind::Enum(v) => {
            let quoted: Vec<String> = v.iter().map(|s| format!("\"{}\"", escape(s))).collect();
            let _ = write!(out, "ENUM {}", quoted.join(","));
        }
    }
    out.push_str(";\n");
}

fn fmt_attr(v: &AttrValue) -> String {
    match v {
        AttrValue::Int(i) => i.to_string(),
        AttrValue::Float(f) => fmt_f64(*f),
        AttrValue::Str(s) => format!("\"{}\"", escape(s)),
    }
}

fn write_value_pairs(out: &mut String, pairs: &[(i64, String)]) {
    for (v, label) in pairs {
        let _ = write!(out, " {} \"{}\"", v, escape(label));
    }
}

/// Format a float the way DBC tools do: integers without a decimal point,
/// everything else in its shortest round-trippable form.
pub fn fmt_f64(v: f64) -> String {
    if v.is_finite() && v.fract() == 0.0 && v.abs() < 1e15 { format!("{}", v as i64) } else { format!("{v}") }
}

fn escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

const DEFAULT_NS: [&str; 28] = [
    "NS_DESC_",
    "CM_",
    "BA_DEF_",
    "BA_",
    "VAL_",
    "CAT_DEF_",
    "CAT_",
    "FILTER",
    "BA_DEF_DEF_",
    "EV_DATA_",
    "ENVVAR_DATA_",
    "SGTYPE_",
    "SGTYPE_VAL_",
    "BA_DEF_SGTYPE_",
    "BA_SGTYPE_",
    "SIG_TYPE_REF_",
    "VAL_TABLE_",
    "SIG_GROUP_",
    "SIG_VALTYPE_",
    "SIGTYPE_VALTYPE_",
    "BO_TX_BU_",
    "BA_DEF_REL_",
    "BA_REL_",
    "BA_DEF_DEF_REL_",
    "BU_SG_REL_",
    "BU_EV_REL_",
    "BU_BO_REL_",
    "SG_MUL_VAL_",
];
