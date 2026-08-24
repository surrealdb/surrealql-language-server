--@ diagnostic: 3 unknown-field
DEFINE TABLE person SCHEMAFULL;
DEFINE FIELD name ON person TYPE string;
SELECT prson_name FROM person;
