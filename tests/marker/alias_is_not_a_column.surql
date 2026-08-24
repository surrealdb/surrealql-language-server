DEFINE TABLE person SCHEMAFULL;
DEFINE FIELD name ON person TYPE string;
SELECT name AS who FROM person WHERE who = 'x';
