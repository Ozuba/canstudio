//! Self-contained DBC model, parser, writer and signal codec.

pub mod codec;
pub mod model;
pub mod parser;
pub mod writer;

pub use codec::{CompiledDb, CompiledMessage, CompiledSignal, SignalRef};
pub use model::*;
pub use parser::{ParseError, parse, parse_lossy};
pub use writer::write;

/// Read a DBC file, decoding non-UTF-8 (cp1252-ish) bytes lossily.
pub fn read_file(path: &std::path::Path) -> std::io::Result<(Dbc, Vec<ParseError>)> {
    let bytes = std::fs::read(path)?;
    let text = match String::from_utf8(bytes) {
        Ok(s) => s,
        // Latin-1 fallback keeps accented characters instead of U+FFFD.
        Err(e) => e.into_bytes().iter().map(|&b| b as char).collect(),
    };
    Ok(parse_lossy(&text))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SOCIALLEDGE: &str = include_str!("../tests/data/socialledge.dbc");

    #[test]
    fn parses_basic_structure() {
        let dbc = parse(SOCIALLEDGE).unwrap();
        assert_eq!(dbc.nodes.len(), 5);
        assert_eq!(dbc.nodes[0].name, "DBG");
        assert_eq!(dbc.messages.len(), 5);

        // Multiplexed message.
        let sonars = dbc.find_message(200).unwrap();
        assert_eq!(sonars.name, "SENSOR_SONARS");
        assert_eq!(sonars.signals.len(), 10);
        let mux = &sonars.signals[0];
        assert_eq!(mux.name, "SENSOR_SONARS_mux");
        assert_eq!(mux.multiplexer, Multiplexer::Multiplexor);
        let left = sonars.signals.iter().find(|s| s.name == "SENSOR_SONARS_left").unwrap();
        assert_eq!(left.multiplexer, Multiplexer::Multiplexed(0));
        let no_filt = sonars.signals.iter().find(|s| s.name == "SENSOR_SONARS_no_filt_left").unwrap();
        assert_eq!(no_filt.multiplexer, Multiplexer::Multiplexed(1));

        // Signed signal with a negative offset.
        let steer = dbc.find_message(101).unwrap().signals.iter().find(|s| s.name == "MOTOR_CMD_steer").unwrap();
        assert_eq!(steer.value_type, ValueType::Signed);
        assert_eq!(steer.offset, -5.0);

        // VAL_ attached to the right signal.
        let cmd = &dbc.find_message(100).unwrap().signals[0];
        assert_eq!(cmd.value_descriptions.len(), 3);
        assert_eq!(cmd.value_descriptions[0], (2, "DRIVER_HEARTBEAT_cmd_REBOOT".to_string()));
    }

    #[test]
    fn comments_survive_trailing_line_comments() {
        // socialledge has `CM_ ...; // trailing` lines that must not swallow the
        // following comment statement.
        let dbc = parse(SOCIALLEDGE).unwrap();
        let motor = dbc.nodes.iter().find(|n| n.name == "MOTOR").unwrap();
        assert_eq!(motor.comment.as_deref(), Some("The motor controller of the car"));
        let driver = dbc.nodes.iter().find(|n| n.name == "DRIVER").unwrap();
        assert_eq!(driver.comment.as_deref(), Some("// The driver controller driving the car //"));
    }

    #[test]
    fn parses_signed_and_factor() {
        let src = r#"VERSION ""
BO_ 100 Test: 8 ECU
 SG_ Current_A : 48|16@1- (0.03278,0) [0|1] "A" Vector__XXX
"#;
        let dbc = parse(src).unwrap();
        let s = &dbc.messages[0].signals[0];
        assert_eq!(s.value_type, ValueType::Signed);
        assert!((s.factor - 0.03278).abs() < 1e-9);
        assert_eq!(s.unit, "A");
    }

    #[test]
    fn round_trip_is_stable() {
        // parse -> write -> parse must yield the same model.
        let dbc1 = parse(SOCIALLEDGE).unwrap();
        let text = write(&dbc1);
        let dbc2 = parse(&text).unwrap();
        assert_eq!(dbc1.messages, dbc2.messages);
        assert_eq!(dbc1.nodes, dbc2.nodes);
        assert_eq!(dbc1.value_tables, dbc2.value_tables);
        assert_eq!(dbc1.version, dbc2.version);
    }

    #[test]
    fn write_then_parse_idempotent() {
        let dbc1 = parse(SOCIALLEDGE).unwrap();
        let t1 = write(&dbc1);
        let dbc2 = parse(&t1).unwrap();
        let t2 = write(&dbc2);
        assert_eq!(t1, t2, "writer must be idempotent");
    }

    #[test]
    fn parses_value_tables_including_empty() {
        let src = r#"VERSION ""
VAL_TABLE_ Table3 16 "16" 7 "7" 2 "2" 0 "0" ;
VAL_TABLE_ Empty ;
VAL_TABLE_ Table1 1 "One" 0 "Zero" ;
"#;
        let dbc = parse(src).unwrap();
        assert_eq!(dbc.value_tables.len(), 3);
        assert_eq!(dbc.value_tables[0].name, "Table3");
        assert_eq!(dbc.value_tables[0].values.len(), 4);
        assert_eq!(dbc.value_tables[1].name, "Empty");
        assert!(dbc.value_tables[1].values.is_empty());
        assert_eq!(dbc.value_tables[2].values, vec![(1, "One".into()), (0, "Zero".into())]);
    }

    #[test]
    fn comments_attach_to_objects() {
        let src = r#"VERSION ""
BU_: ECU
BO_ 100 Test: 8 ECU
 SG_ Sig : 0|8@1+ (1,0) [0|0] "" Vector__XXX
CM_ "network level";
CM_ BU_ ECU "the controller";
CM_ BO_ 100 "a message";
CM_ SG_ 100 Sig "a signal";
"#;
        let dbc = parse(src).unwrap();
        assert_eq!(dbc.comment.as_deref(), Some("network level"));
        assert_eq!(dbc.nodes[0].comment.as_deref(), Some("the controller"));
        assert_eq!(dbc.messages[0].comment.as_deref(), Some("a message"));
        assert_eq!(dbc.messages[0].signals[0].comment.as_deref(), Some("a signal"));
    }

    #[test]
    fn attributes_valtypes_and_ext_mux_round_trip() {
        let src = r#"VERSION ""
BU_: ECU
BO_ 2147483905 Ext: 8 ECU
 SG_ S M : 0|8@1+ (1,0) [0|0] "" Vector__XXX
 SG_ B m2M : 8|8@1+ (1,0) [0|0] "" Vector__XXX
 SG_ C m5 : 16|32@1- (1,0) [0|0] "" Vector__XXX
BA_DEF_ BO_ "GenMsgCycleTime" INT 0 65535;
BA_DEF_ BO_ "VFrameFormat" ENUM "StandardCAN","ExtendedCAN","StandardCAN_FD","ExtendedCAN_FD";
BA_DEF_ SG_ "GenSigStartValue" FLOAT 0 100;
BA_DEF_ "BusType" STRING ;
BA_DEF_DEF_ "GenMsgCycleTime" 0;
BA_DEF_DEF_ "BusType" "CAN";
BA_ "BusType" "CAN FD";
BA_ "GenMsgCycleTime" BO_ 2147483905 100;
BA_ "VFrameFormat" BO_ 2147483905 3;
BA_ "GenSigStartValue" SG_ 2147483905 C 2;
SIG_VALTYPE_ 2147483905 C : 1;
SG_MUL_VAL_ 2147483905 C B 5-7, 9-9;
SG_MUL_VAL_ 2147483905 B S 2-2;
"#;
        let dbc = parse(src).unwrap();
        assert!(dbc.extra.is_empty(), "{:?}", dbc.extra);
        let m = &dbc.messages[0];
        assert_eq!(dbc.cycle_time_ms(m), Some(100.0));
        assert!(dbc.is_fd(m));
        assert_eq!(m.signals[1].multiplexer, Multiplexer::MultiplexedMultiplexor(2));
        assert_eq!(m.signals[2].value_type, ValueType::Float32);
        assert_eq!(m.signals[2].attributes, vec![("GenSigStartValue".into(), AttrValue::Float(2.0))]);
        assert_eq!(m.signals[2].ext_mux.as_ref().unwrap().ranges, vec![(5, 7), (9, 9)]);
        assert_eq!(dbc.attr_display(&dbc.attributes, "BusType").as_deref(), Some("CAN FD"));
        let mut again = parse(&write(&dbc)).unwrap();
        again.new_symbols.clear(); // the writer fills in the default NS_ list
        assert_eq!(again, dbc);
    }

    #[test]
    fn renames_keep_references() {
        let src = r#"VERSION ""
BU_: ECU
BO_ 100 M: 8 ECU
 SG_ S M : 0|8@1+ (1,0) [0|0] "" ECU
 SG_ A m1M : 8|8@1+ (1,0) [0|0] "" ECU
 SG_ B m1 : 16|8@1+ (1,0) [0|0] "" ECU
SIG_GROUP_ 100 G 1 : S A;
SG_MUL_VAL_ 100 B A 1-1;
"#;
        let mut dbc = parse(src).unwrap();
        dbc.rename_signal(0, 1, "Sub");
        dbc.rename_node(0, "VCU");
        dbc.set_message_id(0, 200);
        assert_eq!(dbc.messages[0].transmitter, "VCU");
        assert_eq!(dbc.messages[0].signals[0].receivers, vec!["VCU".to_string()]);
        assert_eq!(dbc.messages[0].signals[2].ext_mux.as_ref().unwrap().switch, "Sub");
        assert_eq!(dbc.extra, vec!["SIG_GROUP_ 200 G 1 : S Sub;".to_string()]);
    }

    #[test]
    fn lossy_parse_reports_and_continues() {
        let src = "BO_ x Bad: 8 X\nBO_ 1 Good: 8 X\n SG_ A : 0|8@1+ (1,0) [0|0] \"\" X\n";
        let (dbc, errors) = parse_lossy(src);
        assert_eq!(errors.len(), 1);
        assert_eq!(dbc.messages.len(), 1);
        assert_eq!(dbc.messages[0].signals.len(), 1);
    }
}
