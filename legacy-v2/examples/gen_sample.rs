//! Generates a small generic spreadsheet as an .xlsx file, so the desktop and
//! web interfaces can be tried without private data.
//!
//! Usage: cargo run --example gen_sample -- <out.xlsx>
//!
//! The data is deterministic, so repeated runs produce the same file.

use rust_xlsxwriter::Workbook;

const HEADERS: [&str; 14] = [
    "Order Date",
    "Invoice Number",
    "Customer",
    "Customer ID",
    "Supplier",
    "SKU",
    "Product Name",
    "Brand",
    "Category",
    "Country",
    "Warehouse",
    "Quantity",
    "Net Weight kg",
    "Value USD",
];

const CUSTOMERS: [(&str, &str); 6] = [
    ("Demo Retail LLC", "C-1001"),
    ("Northwind Market", "C-1002"),
    ("Globex Online", "C-1003"),
    ("Acme Devices", "C-1004"),
    ("Carpathia Stores", "C-1005"),
    ("Blue Harbor Supply", "C-1006"),
];

const SUPPLIERS: [&str; 6] = [
    "Bright Components",
    "Summit Manufacturing",
    "Polar Logistics",
    "Metro Distribution",
    "Vector Goods",
    "Noble Workshop",
];

const PRODUCTS: [(&str, &str, &str, &str, f64); 8] = [
    (
        "SKU-1000",
        "USB-C power adapter",
        "Voltix",
        "Electronics",
        18.0,
    ),
    (
        "SKU-1010",
        "Wireless keyboard",
        "Keylane",
        "Electronics",
        32.0,
    ),
    ("SKU-1020", "Office chair", "Worknest", "Furniture", 95.0),
    ("SKU-1030", "Storage box", "Nordbox", "Household", 8.5),
    ("SKU-1040", "LED desk lamp", "Lumio", "Lighting", 24.0),
    ("SKU-1050", "Cotton t-shirt", "PlainWorks", "Apparel", 7.2),
    ("SKU-1060", "Travel backpack", "Route", "Travel", 41.0),
    ("SKU-1070", "Steel bottle", "Hydra", "Household", 11.0),
];

const COUNTRIES: [&str; 6] = ["CN", "DE", "PL", "TR", "IT", "VN"];
const WAREHOUSES: [&str; 4] = ["Kyiv", "Lviv", "Warsaw", "Berlin"];

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let out = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "sample.xlsx".to_string());

    let mut workbook = Workbook::new();
    let sheet = workbook.add_worksheet();
    for (col, header) in HEADERS.iter().enumerate() {
        sheet.write_string(0, col as u16, *header)?;
    }

    let rows = 220u32;
    for i in 0..rows {
        let r = i + 1;
        let idx = i as usize;
        let (customer, customer_id) = CUSTOMERS[idx % CUSTOMERS.len()];
        let supplier = SUPPLIERS[(idx * 7) % SUPPLIERS.len()];
        let (sku, product, brand, category, base_price) = PRODUCTS[(idx * 3) % PRODUCTS.len()];
        let country = COUNTRIES[(idx * 11) % COUNTRIES.len()];
        let warehouse = WAREHOUSES[(idx * 5) % WAREHOUSES.len()];

        let year = if idx.is_multiple_of(9) { 2025 } else { 2024 };
        let month = (idx % 12) + 1;
        let day = (idx % 27) + 1;
        let date = format!("{year:04}-{month:02}-{day:02}");

        let quantity = 5.0 + (idx % 18) as f64 * 3.0;
        let net_kg = (quantity * (0.4 + (idx % 7) as f64 * 0.35) * 10.0).round() / 10.0;
        let price = base_price * (0.88 + (idx % 9) as f64 * 0.03);
        let value = (price * quantity).round();

        sheet.write_string(r, 0, &date)?;
        sheet.write_string(r, 1, format!("INV-{year}-{:06}", 100000 + i))?;
        sheet.write_string(r, 2, customer)?;
        sheet.write_string(r, 3, customer_id)?;
        sheet.write_string(r, 4, supplier)?;
        sheet.write_string(r, 5, sku)?;
        sheet.write_string(r, 6, product)?;
        sheet.write_string(r, 7, brand)?;
        sheet.write_string(r, 8, category)?;
        sheet.write_string(r, 9, country)?;
        sheet.write_string(r, 10, warehouse)?;
        sheet.write_number(r, 11, quantity)?;
        sheet.write_number(r, 12, net_kg)?;
        sheet.write_number(r, 13, value)?;
    }

    workbook.save(&out)?;
    println!("Wrote {rows} rows to {out}");
    Ok(())
}
