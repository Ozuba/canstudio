//! Consistency checks for an edited database.

use std::collections::HashMap;

use dbc::{Dbc, Multiplexer, ValueType, codec::BitLayout};

use super::widgets::occupied_positions;

pub struct Problem {
    pub msg: Option<usize>,
    pub sig: Option<usize>,
    pub text: String,
}

/// Can two signals of the same message be present in the same frame?
fn coexist(a: &dbc::Signal, b: &dbc::Signal) -> bool {
    match (a.multiplexer.mux_value(), b.multiplexer.mux_value()) {
        (Some(x), Some(y)) => x == y && a.ext_mux.is_none() && b.ext_mux.is_none(),
        _ => true,
    }
}

pub fn check(dbc: &Dbc) -> Vec<Problem> {
    let mut out = Vec::new();
    let mut ids: HashMap<u32, usize> = HashMap::new();
    let mut names: HashMap<&str, usize> = HashMap::new();
    for (mi, m) in dbc.messages.iter().enumerate() {
        let mut p = |sig: Option<usize>, text: String| out.push(Problem { msg: Some(mi), sig, text });
        if let Some(prev) = ids.insert(m.id, mi) {
            p(None, format!("{}: id {:X} also used by {}", m.name, m.raw_id(), dbc.messages[prev].name));
        }
        if let Some(prev) = names.insert(&m.name, mi)
            && prev != mi
        {
            p(None, format!("{}: duplicate message name", m.name));
        }
        if m.size > 64 {
            p(None, format!("{}: length {} exceeds 64 bytes", m.name, m.size));
        }
        if m.raw_id() > if m.is_extended() { 0x1FFF_FFFF } else { 0x7FF } {
            p(None, format!("{}: id out of range for its frame format", m.name));
        }
        let has_switch = m.signals.iter().any(|s| s.multiplexer.is_switch());
        let mut seen: HashMap<&str, usize> = HashMap::new();
        let bits: Vec<Vec<u64>> = m.signals.iter().map(occupied_positions).collect();
        for (si, s) in m.signals.iter().enumerate() {
            let mut p = |text: String| out.push(Problem { msg: Some(mi), sig: Some(si), text: format!("{}.{}: {text}", m.name, s.name) });
            if seen.insert(&s.name, si).is_some() {
                p("duplicate signal name".into());
            }
            match BitLayout::new(s.start_bit, s.size, s.byte_order) {
                None => p(format!("invalid bit layout (start {}, length {})", s.start_bit, s.size)),
                Some(l) if l.required_len() as u64 > m.size => p(format!("needs {} bytes, message has {}", l.required_len(), m.size)),
                _ => {}
            }
            match s.value_type {
                ValueType::Float32 if s.size != 32 => p("Float32 signals must be 32 bits".into()),
                ValueType::Float64 if s.size != 64 => p("Float64 signals must be 64 bits".into()),
                _ => {}
            }
            if s.factor == 0.0 {
                p("factor is zero".into());
            }
            if matches!(s.multiplexer, Multiplexer::Multiplexed(_) | Multiplexer::MultiplexedMultiplexor(_)) && !has_switch {
                p("multiplexed but the message has no multiplexor".into());
            }
            if let Some(e) = &s.ext_mux
                && !m.signals.iter().any(|x| x.name == e.switch)
            {
                p(format!("extended mux switch '{}' does not exist", e.switch));
            }
            for (sj, other) in m.signals.iter().enumerate().skip(si + 1) {
                if coexist(s, other) && bits[si].iter().any(|b| bits[sj].contains(b)) {
                    p(format!("overlaps {}", other.name));
                }
            }
        }
    }
    out
}
