// The golden schema of the Thrift registry kind: documents beside it were
// written by the Apache Thrift 0.25 Python library (TCompactProtocol), or cut
// and patched by hand for the malformed ones.
// tentabus:struct Order
// tentabus:protocol compact
namespace java shop

enum Status {
  NEW = 0,
  PAID = 1,
  SHIPPED = 5,
}

struct Line {
  1: required string sku,
  2: i32 qty,
}

union Payment {
  1: string card,
  2: i64 voucher,
}

struct Order {
  1: required i64 id,
  2: optional string customer,
  3: Status status,
  4: list<Line> lines,
  5: map<string, i32> tags,
  6: set<i16> flags,
  7: bool urgent,
  8: double total,
  9: binary note,
  10: uuid ref,
  11: Payment payment,
  12: byte grade,
  13: list<bool> checks,
}
