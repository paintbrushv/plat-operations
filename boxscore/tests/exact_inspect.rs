//! `inspect_export@1.0.0`: dialect, header, banner and footer detection,
//! column kinds, caps and redaction. Synthetic data only; every name below is
//! invented. The goldens in `tests/fixtures/catalog/inspect_export/` pin whole
//! results; these tests pin each rule.
use boxscore::exact::catalog::{self, Call};
use serde_json::{json, Value};

async fn call(input: Value) -> catalog::Envelope {
    catalog::invoke(&Call {
        function: "inspect_export".into(),
        version: Some("1.0.0".into()),
        input,
        database: None,
        caller: "test".into(),
    })
    .await
}

async fn inspect(text: &str) -> Value {
    inspect_with(json!({"name": "t.csv", "text": text})).await
}

async fn inspect_with(input: Value) -> Value {
    let envelope = call(input).await;
    assert_eq!(envelope.status, "calculated", "{:?}", envelope.error);
    envelope.result.unwrap()
}

async fn refusal(input: Value) -> (String, String) {
    let envelope = call(input).await;
    assert_eq!(envelope.status, "refused", "{:?}", envelope.result);
    let error = envelope.error.unwrap();
    (
        error["code"].as_str().unwrap().to_string(),
        error["message"].as_str().unwrap().to_string(),
    )
}

fn column<'a>(result: &'a Value, name: &str) -> &'a Value {
    result["columns"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == name)
        .unwrap_or_else(|| panic!("no column {name}"))
}

fn kind(result: &Value, name: &str) -> String {
    column(result, name)["kind"].as_str().unwrap().to_string()
}

/// A one-column-under-test table: `label` header plus the values, next to a
/// constant `Unit` column so the table always has two cells per row.
async fn kind_of(label: &str, values: &[&str]) -> String {
    let mut text = format!("Unit,{label}\n");
    for (i, value) in values.iter().enumerate() {
        text.push_str(&format!("U{i},\"{value}\"\n"));
    }
    kind(&inspect(&text).await, label)
}

#[tokio::test]
async fn the_result_is_deterministic() {
    let input = json!({"name": "a.csv", "text": "Unit,Rent\nA1,100.00\nA2,200.00\n"});
    let first = call(input.clone()).await;
    let second = call(input).await;
    assert_eq!(first.result, second.result);
    assert_eq!(first.output_sha256, second.output_sha256);
    assert_eq!(first.version, Some("1.0.0"));
}

#[tokio::test]
async fn delimiters_are_detected_by_consistent_cell_counts() {
    for (delimiter, name) in [(",", ","), ("\t", "\t"), (";", ";"), ("|", "|")] {
        let text = ["Unit", "Rent", "Status"].join(delimiter)
            + "\n"
            + &["A1", "100.00", "Occupied"].join(delimiter)
            + "\n"
            + &["A2", "200.00", "Vacant"].join(delimiter)
            + "\n";
        let result = inspect(&text).await;
        assert_eq!(result["dialect"]["delimiter"], name);
        assert_eq!(result["header"], json!(["Unit", "Rent", "Status"]));
        assert_eq!(result["data_row_count"], 2);
    }
    // A semicolon file whose cells hold commas is still semicolon separated.
    let result = inspect("Unit;Note\nA1;\"one, two, three\"\nA2;plain, text, here\n").await;
    assert_eq!(result["dialect"]["delimiter"], ";");
    assert_eq!(result["dialect"]["quote"], "\"");
    // A single-line title row does not decide the delimiter.
    let result = inspect("Sample Gardens; Income Statement\nUnit\tRent\nA1\t5\nA2\t6\n").await;
    assert_eq!(result["dialect"]["delimiter"], "\t");
    assert_eq!(result["header_row"], 1);
}

#[tokio::test]
async fn quote_and_line_ending_are_reported() {
    let lf = inspect("a,b\n1,2\n").await;
    assert_eq!(lf["dialect"]["line_ending"], "lf");
    assert_eq!(lf["dialect"]["quote"], "none");
    let crlf = inspect("a,b\r\n\"1\",2\r\n").await;
    assert_eq!(crlf["dialect"]["line_ending"], "crlf");
    assert_eq!(crlf["dialect"]["quote"], "\"");
    let mixed = inspect("a,b\r\n1,2\n3,4\r\n").await;
    assert_eq!(mixed["dialect"]["line_ending"], "mixed");
    // A bare carriage return is its own ending, and mixing it with a line feed is mixed.
    let bare = inspect("a,b\r1,2\r").await;
    assert_eq!(bare["dialect"]["line_ending"], "cr");
    assert_eq!(bare["header"], json!(["a", "b"]));
    let mixed = inspect("a,b\r1,2\n3,4\r").await;
    assert_eq!(mixed["dialect"]["line_ending"], "mixed");
}

#[tokio::test]
async fn a_byte_order_mark_is_reported_and_ignored() {
    let plain = inspect("account_code,amount\n4000,1.00\n").await;
    let bom = inspect("\u{FEFF}account_code,amount\n4000,1.00\n").await;
    assert_eq!(plain["bom"], false);
    assert_eq!(plain["encoding_note"], Value::Null);
    assert_eq!(bom["bom"], true);
    assert_eq!(bom["encoding"], "utf-8");
    assert!(bom["encoding_note"].as_str().unwrap().contains("U+FEFF"));
    assert_eq!(bom["header"], json!(["account_code", "amount"]));
    assert_eq!(bom["header_row"], 0);
}

#[tokio::test]
async fn banner_rows_precede_the_header_and_are_capped() {
    let result = inspect(
        "Sample Gardens,,,\nIncome Statement 05/2026,,,\n\nAccount Code,Description,Period,Amount\n4000,Rent,2026-05,10.00\n4010,Fees,2026-05,5.00\n",
    )
    .await;
    assert_eq!(result["header_row"], 2, "blank lines are not rows");
    assert_eq!(
        result["banner_rows"],
        json!(["Sample Gardens", "Income Statement 05/2026"])
    );

    // At most ten banner rows, each at most 200 characters.
    let mut text = String::new();
    for i in 0..12 {
        text.push_str(&format!("{}{i},,\n", "x".repeat(300)));
    }
    text.push_str("A,B,C\n1,2,3\n4,5,6\n");
    let result = inspect(&text).await;
    assert_eq!(result["header_row"], 12);
    let banners = result["banner_rows"].as_array().unwrap();
    assert_eq!(banners.len(), 10);
    assert!(banners
        .iter()
        .all(|b| b.as_str().unwrap().chars().count() == 200));
}

#[tokio::test]
async fn the_header_has_the_usual_cell_count_and_only_labels() {
    // A numeric row is never the header; a short row above the table is a banner.
    let result = inspect("Run 5,x\nUnit,Rent,Status\nA1,100,Occupied\nA2,200,Vacant\n").await;
    assert_eq!(result["header_row"], 1);
    assert_eq!(result["banner_rows"], json!(["Run 5,x"]));
    // An empty header cell disqualifies the row.
    let (code, message) = refusal(json!({"name": "t", "text": ",Rent\nA1,100\nA2,200\n"})).await;
    assert_eq!(code, "INVALID_INPUT");
    assert!(message.contains("no header row"), "{message}");
    // Headers are trimmed.
    let result = inspect("  Unit , Rent \nA1,1\nA2,2\n").await;
    assert_eq!(result["header"], json!(["Unit", "Rent"]));
}

#[tokio::test]
async fn footer_rows_use_the_shared_footer_token_rule() {
    let result = inspect(
        "Unit,Resident,Rent\nA1,Resident A,100.00\nA2,Total Wine LLC,200.00\nSubtotal Floor 1,,300.00\nGrand Total,,300.00\n,,\nTOTAL,,600.00\nSummary of charges,,\nA3,Resident C,50.00\n",
    )
    .await;
    assert_eq!(result["footer_row_count"], 4);
    let rows: Vec<(i64, &str)> = result["footer_rows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| {
            (
                f["row"].as_i64().unwrap(),
                f["first_cell"].as_str().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        rows,
        [
            (3, "Subtotal Floor 1"),
            (4, "Grand Total"),
            (6, "TOTAL"),
            (7, "Summary of charges")
        ]
    );
    // A1, "Total Wine LLC" and A3 are data; the all-empty row is neither.
    assert_eq!(result["data_row_count"], 3);

    // At most 20 footer rows are listed; all are counted.
    let mut text = String::from("Unit,Rent\nA1,1\n");
    for i in 0..25 {
        text.push_str(&format!("Subtotal {i},5\n"));
    }
    let result = inspect(&text).await;
    assert_eq!(result["footer_row_count"], 25);
    assert_eq!(result["footer_rows"].as_array().unwrap().len(), 20);
    assert_eq!(result["data_row_count"], 1);
}

#[tokio::test]
async fn column_kinds() {
    assert_eq!(
        kind_of(
            "Amount",
            &["1,234.50", "(1,200.00)", "300-", "-4.5", "+7", "0.25"]
        )
        .await,
        "money"
    );
    assert_eq!(kind_of("Amount", &["100", "200.00"]).await, "money");
    assert_eq!(kind_of("Beds", &["1", "2", "-3"]).await, "integer");
    assert_eq!(
        kind_of("Moved In", &["2026-05-31", "05/31/2026", "5/1/2026"]).await,
        "date"
    );
    assert_eq!(
        kind_of("Moved In", &["2026-02-30", "x"]).await,
        "text",
        "not a calendar date"
    );
    assert_eq!(
        kind_of("Month", &["2026-05", "Jun 2026", "dec 2025"]).await,
        "period"
    );
    assert_eq!(kind_of("Month", &["2026-13", "Foo 2026"]).await, "text");
    assert_eq!(kind_of("Notes", &["", "", ""]).await, "empty");
    assert_eq!(kind_of("Status", &["Occupied", "Vacant"]).await, "text");
    // An identifier is a label, not a quantity; so is a number with leading zeros.
    assert_eq!(kind_of("Account Code", &["4000", "4010"]).await, "text");
    assert_eq!(kind_of("Unit #", &["101", "102"]).await, "text");
    assert_eq!(kind_of("Zip", &["02134", "10001"]).await, "text");
    // At least 90% decides: 9 of 10 is enough, 8 of 10 is not.
    let nine: Vec<&str> = ["1.00"; 9].into_iter().chain(["n/a"]).collect();
    assert_eq!(kind_of("Amount", &nine).await, "money");
    let eight: Vec<&str> = ["1.00"; 8].into_iter().chain(["n/a", "n/a"]).collect();
    assert_eq!(kind_of("Amount", &eight).await, "text");
    // Text that only resembles a number is text.
    assert_eq!(
        kind_of("Ref", &["1,23", "12.", "--5", "1 000"]).await,
        "text"
    );
}

#[tokio::test]
async fn fill_rate_distinct_count_and_samples() {
    let mut text = String::from("Unit,Note\n");
    for i in 0..50 {
        let note = if i == 7 { "" } else { "same" };
        text.push_str(&format!("U{i},{note}\n"));
    }
    let result = inspect(&text).await;
    assert_eq!(result["data_row_count"], 50);
    let note = column(&result, "Note");
    assert_eq!(note["fill_rate"], "0.98");
    assert_eq!(note["distinct_count"], 1);
    assert_eq!(note["distinct_capped"], false);
    assert_eq!(note["sample"], json!(["same"]));
    assert_eq!(column(&result, "Unit")["fill_rate"], "1.00");

    // Rounds half up to two places: 1 of 8 is 0.125, 1 of 3 is 0.33.
    let result = inspect("A,B\nx,1\ny,\nz,\n").await;
    assert_eq!(column(&result, "B")["fill_rate"], "0.33");
    let result = inspect("A,B\nx,1\ny,\nz,\nw,\nv,\nu,\nt,\ns,\n").await;
    assert_eq!(column(&result, "B")["fill_rate"], "0.13");

    // Five distinct samples in first-seen order, each at most 80 characters.
    let long = "q".repeat(120);
    let result = inspect_with(json!({
        "name": "t.csv", "redact": false,
        "text": format!("Unit,Tag\n1,b\n2,a\n3,b\n4,c\n5,{long}\n6,d\n7,e\n8,f\n"),
    }))
    .await;
    let tag = column(&result, "Tag");
    let sample = tag["sample"].as_array().unwrap();
    assert_eq!(sample.len(), 5);
    assert_eq!(&sample[..3], &[json!("b"), json!("a"), json!("c")]);
    assert_eq!(sample[3].as_str().unwrap().chars().count(), 80);
    assert_eq!(tag["distinct_count"], 7);
}

#[tokio::test]
async fn distinct_count_is_capped_at_a_thousand() {
    for (rows, count, capped) in [(1000, 1000, false), (1001, 1000, true), (1500, 1000, true)] {
        let mut text = String::from("Code,Amount\n");
        for i in 0..rows {
            text.push_str(&format!("{i},1.00\n"));
        }
        let result = inspect(&text).await;
        let code = column(&result, "Code");
        assert_eq!(code["distinct_count"], count, "{rows} rows");
        assert_eq!(code["distinct_capped"], capped, "{rows} rows");
    }
}

#[tokio::test]
async fn redaction_by_header_word_and_by_distinct_ratio() {
    // Every listed header word, in any case, as a word or part of one.
    for header in [
        "Resident",
        "TENANT Name",
        "Full name",
        "Lessee",
        "Occupant 2",
        "E-Mail Address",
        "Email",
        "Phone",
        "SSN",
        "Primary Contact",
        "Mailing Address",
        "Applicant",
        "Guarantor",
        "Resident_ID",
        "tenantname",
    ] {
        let text = format!("Unit,{header}\n1,Resident A\n2,Resident A\n");
        let result = inspect(&text).await;
        let col = column(&result, header);
        assert_eq!(col["redacted"], true, "{header}");
        assert_eq!(col["sample"], json!(["<redacted>"]), "{header}");
        assert_eq!(
            result["redaction"]["columns"],
            json!([{"index": 1}]),
            "{header}"
        );
        assert_eq!(column(&result, "Unit")["redacted"], false);
    }
    // Nearly unique text with at least 8 values is redacted whatever its header...
    for rows in [8, 20, 21] {
        let mut text = String::from("Unit,Comment\n");
        for i in 0..rows {
            text.push_str(&format!("U{i},free text number {i}\n"));
        }
        let result = inspect(&text).await;
        assert_eq!(column(&result, "Comment")["redacted"], true, "{rows} rows");
        // (the unit id column is just as unique, but an identifier is not redacted)
        assert_eq!(column(&result, "Unit")["redacted"], false);
        assert_eq!(
            result["redaction"]["columns"],
            json!([{"index": 1}]),
            "{rows} rows"
        );
    }
    // ...but 7 values are too few to call...
    let mut text = String::from("Unit,Comment\n");
    for i in 0..7 {
        text.push_str(&format!("U{i},free text number {i}\n"));
    }
    assert_eq!(column(&inspect(&text).await, "Comment")["redacted"], false);
    // ...and so is a column in which each value repeats more than three times.
    for (rows, distinct) in [(32, 8), (40, 10), (30, 5)] {
        let mut text = String::from("Unit,Comment\n");
        for i in 0..rows {
            text.push_str(&format!("U{i},free text number {}\n", i % distinct));
        }
        assert_eq!(
            column(&inspect(&text).await, "Comment")["redacted"],
            false,
            "{rows} rows, {distinct} distinct"
        );
    }
}

#[tokio::test]
async fn a_person_header_redacts_any_kind_and_other_headers_keep_money_and_dates() {
    // A phone number column of digits is an integer column; it is still redacted.
    let result = inspect("Unit,Phone,Resident Balance,Moved In Date,Rent\n1,5550100,10.00,2026-01-01,5.00\n2,5550101,20.00,2026-02-01,6.00\n").await;
    assert_eq!(kind(&result, "Phone"), "integer");
    assert_eq!(column(&result, "Phone")["redacted"], true);
    // A person word in the header redacts money and dates as well: a balance
    // or a birth date is about that person.
    assert_eq!(kind(&result, "Resident Balance"), "money");
    assert_eq!(column(&result, "Resident Balance")["redacted"], true);
    assert_eq!(
        column(&result, "Resident Balance")["sample"],
        json!(["<redacted>"])
    );
    // Without a person word, money and dates carry no names and stay visible.
    assert_eq!(column(&result, "Moved In Date")["redacted"], false);
    assert_eq!(column(&result, "Rent")["sample"], json!(["5.00", "6.00"]));
    // An empty column has nothing to redact.
    let result = inspect("Unit,Email\n1,\n2,\n").await;
    assert_eq!(column(&result, "Email")["redacted"], false);
}

#[tokio::test]
async fn redact_false_returns_real_samples() {
    let text = "Unit,Resident\n1,Resident A\n2,Resident B\n";
    let on = inspect(text).await;
    assert_eq!(
        on["redaction"],
        json!({"applied": true, "columns": [{"index": 1}]})
    );
    let off = inspect_with(json!({"name": "t", "text": text, "redact": false})).await;
    assert_eq!(off["redaction"], json!({"applied": false, "columns": []}));
    let resident = column(&off, "Resident");
    assert_eq!(resident["redacted"], false);
    assert_eq!(resident["sample"], json!(["Resident A", "Resident B"]));
    // The default is on.
    let explicit = inspect_with(json!({"name": "t.csv", "text": text, "redact": true})).await;
    assert_eq!(explicit, on);
}

#[tokio::test]
async fn a_redacted_result_holds_no_sample_value_it_should_not() {
    let text =
        "Unit,Resident,Email\n1,Resident Zed,zed@example.test\n2,Resident Yan,yan@example.test\n";
    let envelope = call(json!({"name": "t", "text": text})).await;
    let rendered = serde_json::to_string(&envelope.result).unwrap();
    for secret in [
        "Resident Zed",
        "zed@example.test",
        "Resident Yan",
        "yan@example.test",
    ] {
        assert!(!rendered.contains(secret), "{secret}");
    }
}

#[tokio::test]
async fn bounds_and_refusals() {
    // 256 columns are reported; 257 are refused.
    let wide = |n: usize| {
        let header: Vec<String> = (0..n).map(|i| format!("c{i}")).collect();
        let row: Vec<String> = (0..n).map(|i| i.to_string()).collect();
        format!(
            "{}\n{}\n{}\n",
            header.join(","),
            row.join(","),
            row.join(",")
        )
    };
    let result = inspect(&wide(256)).await;
    assert_eq!(result["columns"].as_array().unwrap().len(), 256);
    let (code, _) = refusal(json!({"name": "t", "text": wide(257)})).await;
    assert_eq!(code, "INPUT_LIMIT");
    // Any row counts, not only the header: a 257-cell row below a narrow
    // header, or a banner line of a thousand commas, is refused as it is read.
    let (code, message) =
        refusal(json!({"name": "t", "text": format!("a,b\n1,2\n{}\n3,4\n", "x,".repeat(256))}))
            .await;
    assert_eq!(code, "INPUT_LIMIT");
    assert!(message.contains("row 2 has 257 cells"), "{message}");
    let (code, _) =
        refusal(json!({"name": "t", "text": format!("{}\na,b\n1,2\n", ",".repeat(1000))})).await;
    assert_eq!(code, "INPUT_LIMIT");
    // 256 cells on a data row are fine.
    let ok = inspect(&format!("a,b\n1,2\n{}\n3,4\n", "x,".repeat(255) + "x")).await;
    assert_eq!(ok["data_row_count"], 3);

    // No table.
    for text in ["", "   \n\n", "just one column\nof words\n"] {
        let (code, message) = refusal(json!({"name": "t", "text": text})).await;
        assert_eq!(
            (code.as_str(), message.as_str()),
            ("INVALID_INPUT", "no table found")
        );
    }

    // Text over 2 MiB.
    let big = format!("a,b\n{}\n", "x".repeat(2 * 1024 * 1024));
    let (code, _) = refusal(json!({"name": "t", "text": big})).await;
    assert_eq!(code, "INPUT_LIMIT");

    // The label: 1-120 characters.
    let (code, _) = refusal(json!({"name": "", "text": "a,b\n1,2\n"})).await;
    assert_eq!(code, "INVALID_INPUT");
    let (code, _) = refusal(json!({"name": "n".repeat(121), "text": "a,b\n1,2\n"})).await;
    assert_eq!(code, "INVALID_INPUT");
    let ok = inspect_with(json!({"name": "n".repeat(120), "text": "a,b\n1,2\n"})).await;
    assert_eq!(ok["name"].as_str().unwrap().len(), 120);

    // Unknown and missing fields, and a non-boolean flag.
    for input in [
        json!({"name": "t", "text": "a,b\n1,2\n", "path": "/etc/passwd"}),
        json!({"text": "a,b\n1,2\n"}),
        json!({"name": "t"}),
        json!({"name": "t", "text": "a,b\n1,2\n", "redact": "yes"}),
    ] {
        let (code, _) = refusal(input).await;
        assert_eq!(code, "INVALID_INPUT");
    }
}

#[tokio::test]
async fn output_is_bounded_whatever_the_input() {
    // 256 columns of long headers and long values, 12 banner rows, 25 footers.
    let long = "z".repeat(250);
    let header: Vec<String> = (0..256).map(|i| format!("{long}{i}")).collect();
    let mut text = String::new();
    for i in 0..12 {
        text.push_str(&format!("{long}{i}\n"));
    }
    text.push_str(&header.join(","));
    text.push('\n');
    for i in 0..6 {
        let row: Vec<String> = (0..256).map(|c| format!("{long}{c}-{i}")).collect();
        text.push_str(&row.join(","));
        text.push('\n');
    }
    for _ in 0..25 {
        text.push_str(&format!("Total{}\n", ",".repeat(255)));
    }
    assert!(text.len() < 2 * 1024 * 1024, "{}", text.len());
    let envelope = call(json!({"name": "t", "text": text, "redact": false})).await;
    assert_eq!(envelope.status, "calculated", "{:?}", envelope.error);
    let result = envelope.result.unwrap();
    let size = serde_json::to_string(&result).unwrap().len();
    assert!(size <= 256 * 1024, "{size}");
    assert_eq!(result["footer_rows"].as_array().unwrap().len(), 20);
    assert_eq!(result["footer_row_count"], 25);
    if result["truncated"] == false {
        // 256 columns x (name 200 + five samples of 80 + fixed fields) plus caps.
        assert_eq!(result["banner_rows"].as_array().unwrap().len(), 10);
        assert!(result["header"].as_array().unwrap().iter().all(|h| h
            .as_str()
            .unwrap()
            .chars()
            .count()
            == 200));
        for column in result["columns"].as_array().unwrap() {
            let sample = column["sample"].as_array().unwrap();
            assert!(sample.len() <= 5);
            assert!(sample
                .iter()
                .all(|s| s.as_str().unwrap().chars().count() == 80));
        }
    }
}

/// The bound is on bytes: multibyte characters, and control characters that
/// are written out as `U+XXXX`, cannot push the result over 256 KiB. Samples
/// go first, then banner rows, then names get shorter, and `truncated` says so.
#[tokio::test]
async fn output_is_bounded_in_bytes_for_multibyte_and_control_characters() {
    let emoji = "\u{1F600}".repeat(210);
    let header: Vec<String> = (0..256).map(|i| format!("{emoji}{i}")).collect();
    let mut text = String::new();
    for i in 0..3 {
        text.push_str(&format!("{emoji}{i}\n"));
    }
    text.push_str(&header.join(","));
    text.push('\n');
    for i in 0..5 {
        let row: Vec<String> = (0..256).map(|c| format!("{emoji}{c}-{i}")).collect();
        text.push_str(&row.join(","));
        text.push('\n');
    }
    assert!(text.len() < 2 * 1024 * 1024, "{}", text.len());
    let result = inspect_with(json!({"name": "t", "text": text, "redact": false})).await;
    let size = serde_json::to_string(&result).unwrap().len();
    assert!(size <= 256 * 1024, "{size}");
    assert_eq!(result["truncated"], true);
    assert_eq!(result["banner_rows"], json!([]));
    assert_eq!(result["columns"].as_array().unwrap().len(), 256);
    assert!(column_samples_are_gone(&result));

    // Control characters: 100 bells are 600 characters of text, cut to 200.
    let bells = "\u{7}".repeat(100);
    let text = format!("Unit,Pa{bells}th\n1,\"x\u{0}y\u{202E}z\"\n2,\"a\u{A0}b\"\n");
    let result = inspect_with(json!({"name": "n\u{1}", "text": text, "redact": false})).await;
    assert_eq!(result["truncated"], false);
    let name = result["header"][1].as_str().unwrap();
    assert_eq!(name.chars().count(), 200);
    assert!(name.starts_with("PaU+0007U+0007"), "{name}");
    assert_eq!(result["name"], "nU+0001");
    let rendered = serde_json::to_string(&result).unwrap();
    assert!(!rendered.contains("\\u0007") && !rendered.contains("\\u0000"));
    let col = &result["columns"][1];
    assert_eq!(col["name"], result["header"][1]);
    assert_eq!(col["sample"], json!(["xU+0000yU+202Ez", "aU+00A0b"]));
}

fn column_samples_are_gone(result: &Value) -> bool {
    result["columns"]
        .as_array()
        .unwrap()
        .iter()
        .all(|c| c["sample"] == json!([]))
}

#[tokio::test]
async fn ragged_rows_count_as_data_and_missing_cells_as_empty() {
    let result = inspect(
        "Unit,Rent,Status\nA1,100\nA2,200,Vacant,extra\nA3,300,Occupied\nA4,400,Occupied\n",
    )
    .await;
    assert_eq!(result["data_row_count"], 4);
    let status = column(&result, "Status");
    assert_eq!(status["fill_rate"], "0.75");
    assert_eq!(status["sample"], json!(["Vacant", "Occupied"]));
}

#[tokio::test]
async fn headers_a_mapping_needs_stay_visible() {
    // `name` alone, or beside a person word, redacts; qualified by a thing in
    // the books it does not.
    for header in [
        "account_name",
        "Account Name",
        "GL Name",
        "Vendor Name",
        "Property Name",
        "Unit Name",
        "Column Name",
        "Report Name",
        "Account Description",
        "Description",
        "Category",
    ] {
        let text = format!("Code,{header}\n1,Gross Potential Rent\n2,Payroll\n");
        let result = inspect(&text).await;
        let col = column(&result, header);
        assert_eq!(col["redacted"], false, "{header}");
        assert_eq!(
            col["sample"],
            json!(["Gross Potential Rent", "Payroll"]),
            "{header}"
        );
        assert_eq!(result["redaction"]["columns"], json!([]), "{header}");
    }
    // A qualifier does not save a person word.
    for header in [
        "Resident Name",
        "Account Contact",
        "Unit Tenant",
        "Vendor Email",
    ] {
        let text = format!("Code,{header}\n1,Resident A\n2,Resident B\n");
        assert_eq!(
            column(&inspect(&text).await, header)["redacted"],
            true,
            "{header}"
        );
    }
}

#[tokio::test]
async fn the_distinct_ratio_rule_skips_identifier_and_label_headers() {
    let rows = |header: &str| {
        let mut text = format!("Amount,{header}\n");
        for i in 0..30 {
            text.push_str(&format!("{i}.00,value {i}\n"));
        }
        text
    };
    for header in [
        "Unit",
        "Unit ID",
        "Account Code",
        "Account",
        "Reference No",
        "Ref",
        "Invoice Number",
        "Account Description",
        "Category",
        "Unit #",
        "GL Code",
    ] {
        let result = inspect(&rows(header)).await;
        assert_eq!(column(&result, header)["redacted"], false, "{header}");
    }
    // Free text such as a memo or a note is still redacted when nearly unique.
    for header in ["Memo", "Notes", "Comment"] {
        let result = inspect(&rows(header)).await;
        assert_eq!(column(&result, header)["redacted"], true, "{header}");
    }
}

// ---- Header redaction (a header cell is echoed like any other cell) -------

const FIRST: [&str; 10] = [
    "Resident", "Person", "Client", "Member", "Guest", "Renter", "Buyer", "Seller", "Agent",
    "Broker",
];

/// An invented person for row `i`: "Resident A", "Person B", ... never a real name.
fn invented(i: usize) -> String {
    let letter = char::from(b'A' + (i % 26) as u8);
    format!("{} {letter}{}", FIRST[i % FIRST.len()], i / 26)
}

fn rendered(result: &Value) -> String {
    serde_json::to_string(result).unwrap()
}

#[tokio::test]
async fn a_ragged_header_is_chosen_and_padded_so_no_data_row_is_echoed_as_header() {
    // The real header has three cells and every data row four. The first data
    // row has the usual cell count, but the shorter label row above it is the
    // header: it is chosen and the missing name is numbered.
    let text = "Unit,Name,Email\n\
        A-101,Resident Zed,zed@example.test,Occupied\n\
        A-102,Resident Yan,yan@example.test,Current\n\
        A-103,Resident Xia,xia@example.test,Notice\n";
    let result = inspect(text).await;
    assert_eq!(result["header_row"], 0);
    assert_eq!(result["header_redacted"], false);
    assert_eq!(
        result["header"],
        json!(["Unit", "Name", "Email", "<unnamed 3>"])
    );
    assert_eq!(result["banner_rows"], json!([]));
    assert_eq!(result["data_row_count"], 3);
    let names: Vec<&str> = result["columns"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["Unit", "Name", "Email", "<unnamed 3>"]);
    assert_eq!(
        result["redaction"]["columns"],
        json!([{"index": 1}, {"index": 2}])
    );
    let out = rendered(&result);
    for value in [
        "Resident Zed",
        "zed@example.test",
        "Resident Yan",
        "yan@example.test",
        "Resident Xia",
        "xia@example.test",
    ] {
        assert!(!out.contains(value), "{value} in {out}");
    }
    // Ragged data with numbers in it (no row has the usual count and is all
    // labels): the short label row is still the header.
    let result = inspect(
        "Unit,Name,Email\nA-101,Resident Zed,zed@example.test,100.00\nA-102,Resident Yan,yan@example.test,200.00\n",
    )
    .await;
    assert_eq!(
        result["header"],
        json!(["Unit", "Name", "Email", "<unnamed 3>"])
    );
    assert!(!rendered(&result).contains("Resident Zed"));
    // redact false describes the file as it is, with the same choice.
    let raw = inspect_with(json!({"name": "t", "text": text, "redact": false})).await;
    assert_eq!(raw["header_redacted"], false);
    assert_eq!(raw["header"][0], "Unit");
}

#[tokio::test]
async fn a_banner_with_two_cells_does_not_redact_the_real_header() {
    for banner in ["Run Date,10/16/2026", "Property:,Sample Gardens"] {
        let mut text = format!("{banner}\nUnit,Resident Name,Status,Email\n");
        for i in 0..30 {
            text.push_str(&format!(
                "A-{},{},Current,person{i}@example.test\n",
                100 + i,
                invented(i)
            ));
        }
        let result = inspect(&text).await;
        assert_eq!(result["header_row"], 1, "{banner}");
        assert_eq!(
            result["header"],
            json!(["Unit", "Resident Name", "Status", "Email"]),
            "{banner}"
        );
        assert_eq!(result["header_redacted"], false, "{banner}");
        assert_eq!(result["banner_rows"], json!([banner]));
        assert_eq!(column(&result, "Resident Name")["redacted"], true);
        assert_eq!(column(&result, "Email")["redacted"], true);
        assert_eq!(column(&result, "Status")["redacted"], false);
        let out = rendered(&result);
        assert!(
            !out.contains("Resident A") && !out.contains("person0@"),
            "{out}"
        );
    }
}

#[tokio::test]
async fn a_header_with_a_blank_cell_is_found_and_no_data_value_is_echoed() {
    let mut text = String::from("Unit,Resident Name,,Email\n");
    for i in 0..30 {
        text.push_str(&format!(
            "A-{},{},Current,person{i}@example.test\n",
            100 + i,
            invented(i)
        ));
    }
    let result = inspect(&text).await;
    // The blank cell no longer disqualifies the real header.
    assert_eq!(result["header_row"], 0);
    assert_eq!(
        result["header"],
        json!(["Unit", "Resident Name", "", "Email"])
    );
    assert_eq!(result["header_redacted"], false);
    assert_eq!(result["data_row_count"], 30);
    let out = rendered(&result);
    for i in 0..30 {
        assert!(!out.contains(&invented(i)), "{}", invented(i));
        assert!(!out.contains(&format!("person{i}@")), "person{i}");
    }
}

#[tokio::test]
async fn a_header_cell_that_looks_like_data_is_replaced_on_its_own() {
    let long = "x".repeat(41);
    let text = format!(
        "Code,Note jane@example.test,Unit 5550100,{long},2025,2026-01,{}\nA,1,2,3,4,5,6\nB,1,2,3,4,5,6\n",
        "y".repeat(40)
    );
    let result = inspect(&text).await;
    assert_eq!(result["header_row"], 0);
    assert_eq!(result["header_redacted"], true);
    let header = result["header"].as_array().unwrap();
    assert_eq!(header[0], "Code");
    assert_eq!(header[1], "<redacted header 1>", "an @");
    assert_eq!(header[2], "<redacted header 2>", "mostly digits");
    assert_eq!(header[3], "<redacted header 3>", "over 40 characters");
    assert_eq!(header[4], "2025", "a year is a label");
    assert_eq!(header[5], "2026-01", "a period is a label");
    assert_eq!(header[6], "y".repeat(40), "40 characters is not over");
    for (i, h) in header.iter().enumerate() {
        assert_eq!(result["columns"][i]["name"], *h);
    }
    let out = rendered(&result);
    assert!(!out.contains("jane@") && !out.contains("5550100") && !out.contains(&long));
    // A banner cell that is an email address is not echoed either.
    let result = inspect("Contact jane@example.test,,\nUnit,Rent,Status\nA1,1,x\nA2,2,y\n").await;
    assert!(!rendered(&result).contains("jane@"));
}

// ---- PII bypasses ---------------------------------------------------------

/// A two-column table: the constant `Unit` and `header` over `values`.
fn table(header: &str, values: &[String]) -> String {
    let mut text = format!("Unit,\"{header}\"\n");
    for (i, value) in values.iter().enumerate() {
        text.push_str(&format!("U{i},\"{value}\"\n"));
    }
    text
}

#[tokio::test]
async fn classic_pii_headers_are_redacted_whatever_the_column_holds() {
    let kinds: [(&str, Vec<String>); 4] = [
        (
            "integer",
            (0..3).map(|i| format!("{}", 5_550_100 + i)).collect(),
        ),
        ("date", (1..4).map(|i| format!("1980-01-0{i}")).collect()),
        ("money", (1..4).map(|i| format!("{i}.50")).collect()),
        ("text", (0..3).map(invented).collect()),
    ];
    for header in [
        "Tax ID",
        "TaxID",
        "SS No",
        "SSN",
        "Social Security No",
        "TIN",
        "Tenant DOB",
        "DOB",
        "Applicant Birth Date",
        "Date of Birth",
        "Card Number",
        "Credit Card",
        "Bank Account Number",
        "Routing Number",
        "Driver License",
        "Licence No",
        "Mobile",
        "Cell",
        "Cell Phone",
        "Tel",
        "FirstName",
        "First",
        "Last",
        "LastName",
        "Full",
        "FullName",
        "Customer",
        "Payee",
        "Owner",
        "Co-Owner Name",
        "Household",
        "Household Income",
        "Account Holder Name",
        "Cardholder",
        "Name / Unit",
        "E-Mail",
        "Name",
        "name of person",
    ] {
        for (label, values) in &kinds {
            let result = inspect(&table(header, values)).await;
            let col = column(&result, header);
            assert_eq!(col["redacted"], true, "{header} / {label}");
            assert_eq!(col["sample"], json!(["<redacted>"]), "{header} / {label}");
            assert_eq!(column(&result, "Unit")["redacted"], false);
        }
    }
    // The short words match as words only: nothing here names a person.
    for header in [
        "Release Date",
        "Lease",
        "Lease Start",
        "Lease End Date",
        "Hotel",
        "Tinted",
        "Fullerton Plan",
        "Lastly",
        "Cellar",
        "Mail Room",
    ] {
        let missed = header == "Mail Room";
        let result = inspect(&table(header, &kinds[1].1)).await;
        assert_eq!(
            column(&result, header)["redacted"],
            missed,
            "{header} (mail is a word of its own)"
        );
    }
    // Headers a mapping needs stay visible.
    for header in [
        "account_name",
        "Account Description",
        "Vendor Name",
        "GL Name",
    ] {
        let values: Vec<String> = vec!["Gross Potential Rent".into(), "Payroll".into()];
        let result = inspect(&table(header, &values)).await;
        assert_eq!(column(&result, header)["redacted"], false, "{header}");
        assert_eq!(column(&result, header)["sample"], json!(values), "{header}");
    }
}

#[tokio::test]
async fn people_are_redacted_by_the_shape_of_the_column_whatever_its_header() {
    let names = |n: usize| (0..n).map(invented).collect::<Vec<_>>();
    // Names under a header that says nothing: 8 values are enough, 7 are not.
    assert_eq!(
        column(&inspect(&table("Col1", &names(8))).await, "Col1")["redacted"],
        true
    );
    assert_eq!(
        column(&inspect(&table("Col1", &names(7))).await, "Col1")["redacted"],
        false
    );
    // Exactly 20 rows (the old floor was more than 20).
    assert_eq!(
        column(&inspect(&table("Col1", &names(20))).await, "Col1")["redacted"],
        true
    );
    // A rent roll that repeats each resident on three rows, one per charge.
    let mut text = String::from("Unit,Occupied By,Charge,Amount\n");
    for i in 0..60 {
        text.push_str(&format!(
            "{},{},{},10.00\n",
            100 + i / 3,
            invented(i / 3),
            ["RENT", "PET", "PARK"][i % 3]
        ));
    }
    let result = inspect(&text).await;
    assert_eq!(column(&result, "Occupied By")["redacted"], true);
    assert_eq!(column(&result, "Charge")["redacted"], false);
    assert_eq!(column(&result, "Unit")["redacted"], false);
    assert!(!rendered(&result).contains("Resident A0"));
    // A column of unit-qualified names is not exempt from the ratio rule: only
    // `account` and `gl` keep a name column readable.
    assert_eq!(
        column(
            &inspect(&table("Name / Unit", &names(30))).await,
            "Name / Unit"
        )["redacted"],
        true
    );
    // Integers of seven or more digits that differ from row to row are ids of
    // a person, an account or a card: redacted under any header.
    let long: Vec<String> = (0..8).map(|i| format!("{}", 123_456_780 + i)).collect();
    let result = inspect(&table("Col2", &long)).await;
    assert_eq!(kind(&result, "Col2"), "integer");
    assert_eq!(column(&result, "Col2")["redacted"], true);
    let short: Vec<String> = (0..8).map(|i| format!("{}", 100_000 + i)).collect();
    assert_eq!(
        column(&inspect(&table("Col2", &short)).await, "Col2")["redacted"],
        false
    );
}

#[tokio::test]
async fn an_email_a_social_security_number_or_a_card_number_is_redacted_on_sight() {
    // Any header, any row count: one such value redacts the whole column.
    for (header, values) in [
        (
            "Remarks",
            vec!["fine", "write to resident.a@example.test", "fine"],
        ),
        ("Remarks", vec!["fine", "000-00-0000", "fine"]),
        ("Remarks", vec!["see 000-00-0000 on file"]),
        ("Col3", vec!["4000000000000002", "4000000000000010"]),
        ("Col3", vec!["4000 0000 0000 0002", "x"]),
        ("Col3", vec!["4000-0000-0000-0002"]),
        ("Ref", vec!["4000000000000002000"]),
    ] {
        let values: Vec<String> = values.iter().map(|v| v.to_string()).collect();
        let result = inspect(&table(header, &values)).await;
        assert_eq!(
            column(&result, header)["redacted"],
            true,
            "{header} {values:?}"
        );
        let out = rendered(&result);
        assert!(
            !out.contains("resident.a@") && !out.contains("000-00-0000"),
            "{out}"
        );
        assert!(
            !out.contains("4000000000000002") && !out.contains("4000 0000"),
            "{out}"
        );
    }
    // Twelve digits, twenty digits, and a date are none of those.
    for value in [
        "400000000002",
        "40000000000000000002",
        "2026-01-02",
        "12-34-5678",
    ] {
        let values = vec![value.to_string(), value.to_string()];
        let result = inspect(&table("Col3", &values)).await;
        assert_eq!(column(&result, "Col3")["redacted"], false, "{value}");
    }
    // redact false turns the rule off with the rest.
    let text = table("Remarks", &["a@example.test".to_string()]);
    let raw = inspect_with(json!({"name": "t", "text": text, "redact": false})).await;
    assert_eq!(column(&raw, "Remarks")["sample"], json!(["a@example.test"]));
}

// ---- Header detection -----------------------------------------------------

#[tokio::test]
async fn header_detection_allows_blank_cells_years_and_header_only_files() {
    // A blank header cell is fine when at least half are labels and two are filled.
    let result = inspect("Account,,Amount\n4000,Rent,100.00\n4010,Vacancy,5.00\n").await;
    assert_eq!(result["header_row"], 0);
    assert_eq!(result["header"], json!(["Account", "", "Amount"]));
    assert_eq!(result["data_row_count"], 2);
    // One label of three is not half, and two blanks leave one filled cell.
    for text in [
        "Account,,\n4000,Rent,1\n4010,Vacancy,2\n",
        ",,Amount\n4000,Rent,100.00\n",
    ] {
        let (code, message) = refusal(json!({"name": "t", "text": text})).await;
        assert_eq!(code, "INVALID_INPUT");
        assert!(message.contains("no header row"), "{message}");
    }
    // A four-digit year is a label when the values below it are numbers.
    let result =
        inspect("Account,Description,2025,2026\n4000,Rent,100.00,110.00\n4010,Vacancy,5.00,6.00\n")
            .await;
    assert_eq!(result["header_row"], 0);
    assert_eq!(
        result["header"],
        json!(["Account", "Description", "2025", "2026"])
    );
    assert_eq!(kind(&result, "2025"), "money");
    // ...and a number when they are not: a year over text is no header cell.
    let result = inspect("2025,2026\nx,y\nx,y\n").await;
    assert_eq!(result["header_row"], 1, "the years row is not a header");
    // A file that is only a header: that row is the header, with no data.
    let result = inspect("Account,Description,Amount\n").await;
    assert_eq!(result["header_row"], 0);
    assert_eq!(result["data_row_count"], 0);
    assert_eq!(
        result["header"],
        json!(["Account", "Description", "Amount"])
    );
    assert_eq!(kind(&result, "Amount"), "empty");
    assert_eq!(column(&result, "Amount")["fill_rate"], "0.00");
    let result = inspect("Sample Gardens,,\nAccount,Description,Amount\n").await;
    assert_eq!(result["header_row"], 1);
    assert_eq!(result["banner_rows"], json!(["Sample Gardens"]));
    // A tie between two cell counts goes to the header's own count.
    let result =
        inspect("a,b,c,d\n1,2,3,4\n1,2,3,4\n1,2,3,4\n1,2,3,4,5\n1,2,3,4,5\n1,2,3,4,5\n").await;
    assert_eq!(result["header_row"], 0);
    assert_eq!(result["data_row_count"], 6);
}

#[tokio::test]
async fn money_may_carry_a_dollar_sign_inside_or_outside_parentheses() {
    assert_eq!(
        kind_of(
            "Amount",
            &["$1,200.00", "$ (1,200.00)", "(1,200.00)", "12-"]
        )
        .await,
        "money"
    );
    assert_eq!(
        kind_of(
            "Amount",
            &[
                "($1,200.00)",
                "1,200.00 $",
                "-$5.00",
                "$-5.00",
                "$12-",
                "$ 7"
            ]
        )
        .await,
        "money"
    );
    // A whole number with a currency symbol is money, not an integer.
    assert_eq!(kind_of("Amount", &["$100", "$200"]).await, "money");
    // A symbol alone, or two, is not a number.
    assert_eq!(kind_of("Amount", &["$", "$$5"]).await, "text");
    // A header cell that is a price is numeric, so it is not a label.
    let (code, _) = refusal(json!({"name": "t", "text": "$100,$200\n1,2\n"})).await;
    assert_eq!(code, "INVALID_INPUT");
}

#[tokio::test]
async fn account_and_unit_columns_of_whole_numbers_are_text() {
    assert_eq!(kind_of("Account", &["4000", "4010"]).await, "text");
    assert_eq!(kind_of("GL Account", &["4000", "4010"]).await, "text");
    assert_eq!(kind_of("Unit", &["101", "102"]).await, "text");
    assert_eq!(kind_of("Beds", &["1", "2"]).await, "integer");
}

#[tokio::test]
async fn a_second_byte_order_mark_is_refused() {
    let (code, message) = refusal(json!({"name": "t", "text": "\u{FEFF}\u{FEFF}a,b\n1,2\n"})).await;
    assert_eq!(code, "INVALID_INPUT");
    assert!(message.contains("U+FEFF"), "{message}");
    // One mark is still fine, and a mark inside a cell is the cell's business.
    let ok = inspect("\u{FEFF}a,b\n1,2\n").await;
    assert_eq!(ok["bom"], true);
}

#[tokio::test]
async fn banner_rows_keep_their_delimiter_and_hide_control_characters() {
    let result = inspect("Sample\tGar\u{7}dens\tZ\nUnit\tRent\nA1\t5\nA2\t6\n").await;
    assert_eq!(result["header_row"], 1);
    assert_eq!(result["banner_rows"], json!(["Sample\tGarU+0007dens\tZ"]));
}

#[tokio::test]
async fn name_is_qualified_only_when_the_qualifier_directly_precedes_it() {
    let names = |n: usize| (0..n).map(invented).collect::<Vec<_>>();
    // Qualified: visible, and exempt from the distinct-ratio rule too.
    for header in [
        "Vendor Name",
        "Unit Name",
        "Account Name",
        "account_name",
        "AccountName",
        "Property-Name",
        "GL Name",
        "Column Name",
        "Report  Name",
        "MyAccountName",
    ] {
        let few = vec!["Gross Potential Rent".to_string(), "Payroll".to_string()];
        let result = inspect(&table(header, &few)).await;
        assert_eq!(column(&result, header)["redacted"], false, "{header}");
        assert_eq!(column(&result, header)["sample"], json!(few), "{header}");
        let result = inspect(&table(header, &names(30))).await;
        assert_eq!(
            column(&result, header)["redacted"],
            false,
            "{header} with 30 distinct values"
        );
    }
    // Not directly preceded, or no qualifier: a person's name column.
    for header in [
        "Name",
        "Name / Unit",
        "Unit / Name Holder",
        "Name (Account)",
        "Community Name",
        "FirstName",
        "Vendor Contact Name",
        "Vendor Name and Name",
    ] {
        let result = inspect(&table(header, &names(2))).await;
        assert_eq!(column(&result, header)["redacted"], true, "{header}");
    }
}
