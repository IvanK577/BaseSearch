//! Generates a small, realistic customs-style .xlsx for local smoke testing.
//! Usage: cargo run --example make_sample -- <out.xlsx> [rows]

use rust_xlsxwriter::Workbook;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let out = args.next().unwrap_or_else(|| "sample.xlsx".to_string());
    let rows: usize = args.next().and_then(|v| v.parse().ok()).unwrap_or(320);

    let headers = [
        "Date",
        "Recipient",
        "EDRPOU",
        "Sender",
        "Product code",
        "Description",
        "Trademark",
        "Origin country",
        "Dispatch country",
        "Trade country",
        "Net kg",
        "Gross kg",
        "Value USD",
        "Quantity",
    ];

    let recipients = [
        ("TechnoImport LLC", "31005420"),
        ("Global Foods Ukraine", "40123765"),
        ("BuildMaster Group", "38290155"),
        ("MediPharm Distribution", "42551908"),
        ("AutoParts Direct", "35120744"),
    ];
    let senders = [
        "Shenzhen Electronics Co",
        "Bavaria Handels GmbH",
        "Istanbul Textile AS",
        "Krakow Logistics Sp",
        "Milano Foods SRL",
    ];
    let products = [
        ("8471300000", "Portable computers", "Lenovo"),
        ("0901210000", "Roasted coffee", "Lavazza"),
        ("6403990000", "Leather footwear", "Ecco"),
        ("3004900000", "Medicaments retail", "Bayer"),
        ("8708299000", "Vehicle body parts", "Bosch"),
    ];
    let origins = ["CN", "DE", "TR", "PL", "IT"];

    let mut workbook = Workbook::new();
    let sheet = workbook.add_worksheet();
    for (col, header) in headers.iter().enumerate() {
        sheet.write_string(0, col as u16, *header)?;
    }

    let mut seed: u64 = 0x9e3779b97f4a7c15;
    let mut next = || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };

    for i in 0..rows {
        let r = (i + 1) as u32;
        let rec = recipients[(next() as usize) % recipients.len()];
        let sender = senders[(next() as usize) % senders.len()];
        let prod = products[(next() as usize) % products.len()];
        let origin = origins[(next() as usize) % origins.len()];
        let month = 1 + (next() % 12) as u32;
        let day = 1 + (next() % 27) as u32;
        let net = 50.0 + (next() % 5000) as f64 / 10.0;
        let gross = net * 1.08;
        let value = net * (3.0 + (next() % 40) as f64);
        let qty = 1 + (next() % 500);

        sheet.write_string(r, 0, format!("2024-{month:02}-{day:02}"))?;
        sheet.write_string(r, 1, rec.0)?;
        sheet.write_string(r, 2, rec.1)?;
        sheet.write_string(r, 3, sender)?;
        sheet.write_string(r, 4, prod.0)?;
        sheet.write_string(r, 5, prod.1)?;
        sheet.write_string(r, 6, prod.2)?;
        sheet.write_string(r, 7, origin)?;
        sheet.write_string(r, 8, origin)?;
        sheet.write_string(r, 9, origin)?;
        sheet.write_number(r, 10, net)?;
        sheet.write_number(r, 11, gross)?;
        sheet.write_number(r, 12, value)?;
        sheet.write_number(r, 13, qty as f64)?;
    }

    workbook.save(&out)?;
    println!("Wrote {rows} rows to {out}");
    Ok(())
}
