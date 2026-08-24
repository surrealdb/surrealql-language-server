DEFINE TABLE person SCHEMAFULL;
DEFINE FIELD name ON person TYPE string;
-- surql-ignore: unknown-field
SELECT prson_name FROM person;
