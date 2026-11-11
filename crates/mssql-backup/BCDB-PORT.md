# Code derived from bcdb

Some modules of this crate are Rust ports of logic from **bcdb 0.2.1**
(<https://github.com/StefanMaron/BusinessCentral.DbReader>, `src/BcDb.Core`),
Copyright (c) 2026 Stefan Maron, MIT License. The licence text is in
[`LICENSE-BCDB-MIT`](LICENSE-BCDB-MIT) and is named in the repository `NOTICE`.

Rules, enforced by `tests/bcdb_port_headers.rs`:

- A derived file starts with one header line (within its first 20 lines):
  `// Derived from bcdb 0.2.1, src/BcDb.Core/<File>.cs. Copyright (c) 2026 Stefan Maron, MIT License; see crates/mssql-backup/LICENSE-BCDB-MIT.`
  The line names every bcdb file listed for it below. The release export keeps comments that carry a
  copyright marker, so this one line is what survives into the public tree.
- Every derived file has a row in the table; every row names an existing file that carries the header.
- No other source file mentions bcdb. Own code that follows only a documented fact (for example the
  layout of a page) carries no header and is not listed.
- `NOTICE` names the licence when the table has rows, and not before.

| Rust file | bcdb 0.2.1 file | what is derived |
|---|---|---|
| `src/bak_pagemap.rs` | `src/BcDb.Core/PageFile.cs`, `src/BcDb.Core/Mtf.cs` | positional placement of the data copy: per GAM interval the extents named by the GAM, SGAM and PFS-page rules, region-2 frame placement by consensus, filler tail, the PFS allocated bit; which MQDA streams count as data regions |
| `src/bak_record.rs` | `src/BcDb.Core/Records.cs`, `src/BcDb.Core/PageFile.cs` | page header field offsets, the slot array with its empty slots, record kinds and ghost records from the status bits, the cells of a FixedVar record by physical layout (null bits counted from 1, fixed offsets from the start of the record, bit columns by bit position, variable columns by ordinal with trailing empty ones left out, columns added after the row was written) |
| `src/bak_values.rs` | `src/BcDb.Core/Values.cs`, `src/BcDb.Core/Lob.cs`, `src/BcDb.Core/Catalog.cs` | the stored forms of integers, `datetime`, `datetime2`, `date`, `time`, `numeric`, GUID and UTF-16 text; the 16-byte text pointer and the 12 + 12n byte inline root of a LOB or row-overflow column; the system type, length, precision and scale from the `ti` word of a `sysrscols` row and the storage width of each type |
| `src/bak_table.rs` | `src/BcDb.Core/TableReader.cs` | the pages of a table from the IAM chain of its allocation unit (header record with the interval start and eight single-page slots, extent bitmap) filtered by the PFS allocated bit, which page types an extent can hold, the layout of a `sysallocunits` record; rows decoded strictly by the physical layout |
| `src/bak_lob.rs` | `src/BcDb.Core/Lob.cs` | the records of large values on text-mix and text-tree pages (small value, piece, root record, tree record, NULL root: their fields and the cumulative size of every link), the walk from a text pointer or an inline root to the pieces, and the type-8 NULL root |
| `src/bak_catalog.rs` | `src/BcDb.Core/Catalog.cs` | the pointer to the first `sysallocunits` page at offset 612 of the boot page and the walk of that chain; the fixed rowset id of `sysrowsets`; the field offsets of the records of `sysallocunits`, `sysrowsets`, `sysschobjs`, `syscolpars` and `sysrscols`; the physical layout from `sysrscols` with its flags for dropped and internal columns and the uniquifier |
