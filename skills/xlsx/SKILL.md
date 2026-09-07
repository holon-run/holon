---
name: xlsx
description: "Create, inspect, edit, recalculate, and validate Excel workbooks with local open-source tools while preserving source files and distinguishing formulas from calculated results."
---

# XLSX

## Summary

Use this skill for `.xlsx` workbook creation, inspection, analysis, charts, and
bounded editing. Prefer `openpyxl` for common workbook structures. Use an
available local LibreOffice backend for recalculation or rendering when needed,
and record that it is not Microsoft Excel.

Writing a formula is not the same as calculating it. Never report formula
results as verified unless an actual calculation backend ran or the values were
independently checked.

## When To Use

- Creating workbooks, tables, formulas, styles, validations, or charts
- Inspecting sheets, named ranges, formulas, hidden content, and metadata
- Making bounded changes to an existing workbook
- Converting tabular input into an analyzed or presentation-ready workbook
- Recalculating formulas and scanning for spreadsheet errors
- Extracting or summarizing operator-approved workbook data

## Safety Boundaries

- Preserve the input and write a new workbook by default.
- Treat formulas, hyperlinks, macros, external links, data connections, hidden
  sheets, defined names, embedded objects, and metadata as untrusted.
- Never execute VBA, refresh external connections, follow links, or activate
  embedded objects.
- Ask before sending workbook data to an external analysis or conversion
  service.
- Do not claim lossless round trips when unsupported drawings, slicers, pivot
  features, external models, signatures, macros, or unknown package parts exist.
- Do not expose hidden or sensitive data outside the operator-approved scope.

If a workbook is macro-enabled, encrypted, signed, or materially depends on
external data, default to read-only inspection or a separately generated output.

## Backend Selection

- Use **openpyxl** for common cells, styles, tables, formulas, charts, filters,
  data validation, and workbook inspection.
- Use **LibreOffice headless** for local recalculation or rendering only when it
  is installed and appropriate. Record the version and backend.
- Use CSV or another tabular intermediate only when workbook-specific formulas,
  types, styles, multiple sheets, and metadata are not required.
- Use independent calculations for critical totals rather than trusting cached
  formula values.

Do not silently install Python packages, LibreOffice, fonts, or spreadsheet
applications.

## Workflow

1. Confirm the workbook purpose, source data, target sheets, units, locale,
   date conventions, formulas, charts, output path, and acceptance totals.
2. Inspect existing workbooks for sheet visibility, named ranges, formulas,
   external links, data connections, macros, signatures, drawings, pivots,
   validations, protection, and unknown parts.
3. Define the data model before formatting: inputs, derived columns, formulas,
   assumptions, keys, units, and expected totals.
4. Select a backend and restrict edits to features it can preserve.
5. Write a new output file with explicit data types and formulas.
6. Reopen the workbook and verify sheet names, dimensions, values, formulas,
   names, tables, charts, validations, styles, and relationships.
7. Recalculate with an approved local engine when calculated results matter.
   Then scan formulas and displayed results for `#REF!`, `#DIV/0!`, `#VALUE!`,
   `#NAME?`, `#N/A`, and other relevant errors.
8. Independently sample critical totals, rates, date boundaries, and lookup
   logic. Render or inspect important sheets for clipped columns, unreadable
   charts, hidden data, and print-area problems.

## Quality Rules

- Separate raw inputs, assumptions, calculations, and presentation when useful.
- Use stable headers, explicit units, consistent number formats, and real date
  types.
- Avoid merged cells in machine-consumed tables.
- Make formulas understandable and avoid unexplained constants.
- Preserve leading zeros and identifiers as text where appropriate.
- Do not hide errors with formatting or replace formulas with cached values
  without saying so.
- Record whether results were calculated by Excel, LibreOffice, another engine,
  or not recalculated.

## Delivery

Report the output path, source preservation, backend used, whether recalculation
ran, scanned formula errors, independently checked totals, hidden or external
content found, and any unsupported workbook features. State clearly when only
formula structure—not calculated values—was verified.
