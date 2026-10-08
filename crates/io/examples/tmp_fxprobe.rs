use photosuite_psd::descriptor::{Descriptor, Id, Value, VersionedDescriptor};
use std::collections::BTreeMap;
fn id(i: &Id, m: &mut BTreeMap<String, (usize, usize)>) {
    let s = String::from_utf8_lossy(i.as_bytes()).into_owned();
    if s.len() == 4 {
        let e = m.entry(s).or_default();
        if matches!(i, Id::Str(_)) { e.0 += 1 } else { e.1 += 1 }
    }
}
fn desc(d: &Descriptor, m: &mut BTreeMap<String, (usize, usize)>, depth: usize) {
    if depth > 30 { return }
    id(&d.class_id, m);
    for (k, v) in &d.items { id(k, m); val(v, m, depth + 1) }
}
fn val(v: &Value, m: &mut BTreeMap<String, (usize, usize)>, depth: usize) {
    match v {
        Value::Descriptor(d) | Value::GlobalObject(d) => desc(d, m, depth),
        Value::List(l) => l.iter().for_each(|x| val(x, m, depth + 1)),
        Value::Enumerated { type_id, value } => { id(type_id, m); id(value, m) }
        Value::ObjectArray(a) => desc(&a.body, m, depth),
        Value::Class(c) | Value::GlobalClass(c) => id(&c.class_id, m),
        _ => {}
    }
}
fn scan(data: &[u8], m: &mut BTreeMap<String, (usize, usize)>) {
    let mut i = 0;
    while i + 4 <= data.len().min(4096) {
        if data[i..i + 4] == [0, 0, 0, 16] {
            if let Ok((d, n)) = VersionedDescriptor::parse_prefix(&data[i..]) {
                if n > 20 { desc(&d.descriptor, m, 0); i += n; continue; }
            }
        }
        i += 1;
    }
}
fn main() {
    let mut m = BTreeMap::new();
    for p in std::env::args().skip(1) {
        let Ok(b) = std::fs::read(&p) else { continue };
        let Ok(f) = photosuite_psd::PsdFile::from_bytes(&b) else { continue };
        for l in f.layers() { for b in &l.blocks { if !matches!(&b.key, b"lnk2" | b"lnk3" | b"Txt2") { scan(&b.data, &mut m) } } }
        for b in &f.global_blocks { if !matches!(&b.key, b"lnk2" | b"lnk3" | b"Txt2" | b"Patt" | b"FEid") { scan(&b.data, &mut m) } }
        for r in &f.resources { scan(&r.data, &mut m) }
    }
    for (k, (s, c)) in m { if s > 0 { println!("{k:?} str={s} code={c}") } }
}
