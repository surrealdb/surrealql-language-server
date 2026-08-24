--@ settings: { "analysis": { "schemalessDiagnostics": "strict" } }
--@ diagnostic: 5 unknown-field
--@ diagnostic: 5 permission-unknown
DEFINE TABLE person SCHEMALESS;
DEFINE FIELD name ON person TYPE string;
CREATE person SET nickname = 'b';
