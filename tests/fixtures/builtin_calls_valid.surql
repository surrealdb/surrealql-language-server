-- Valid builtin calls taken verbatim from SurrealDB's own language tests.
-- Every file they came from declares no expected error, so each call below
-- is code the engine accepts. The argument checks must stay silent here.
-- Regenerate by re-running the extraction in tests/conformance.rs's doc comment.

-- array::
array::boolean_and([false, true, false, true], [false, false, true, true]);
array::boolean_or([false, true, false, true], [false, false, true, true]);
array::boolean_xor([false, true, false, true], [false, false, true, true]);
array::fill([], 10);
array::logical_and([true, false, true, false], [true, true, false, false]);
array::logical_or([true, false, true, false], [true, true, false, false]);
array::logical_xor([true, false, true, false], [true, true, false, false]);
array::matches([0, 1, 2], 1);
array::remove([3], 0);
array::transpose([[0, 1], [2, 3]]);
SELECT array::complement(<->knows<->person, [id]) AS connections FROM person:alice;
SELECT tags.{id,name} FROM person WHERE array::len(tags) > 1;
SELECT array::join(txt," ") FROM tgt GROUP ALL;
SELECT user, array::group(role) FROM test GROUP BY user;

-- crypto::
CREATE user:bob SET email = "bob@example.com", pass = crypto::argon2::generate("bob-secret");
crypto::argon2::compare(crypto::argon2::generate('p4ssw0rd'), 'p4ssw0rd');
crypto::bcrypt::compare(crypto::bcrypt::generate('p4ssw0rd'), 'p4ssw0rd');
crypto::blake3('tobie');
crypto::joaat('tobie');
crypto::md5('tobie');
crypto::sha1('tobie');
crypto::sha256('tobie');
crypto::sha512('tobie');

-- duration::
duration::days(7d);
duration::hours(7h);
duration::micros(150µs);
duration::millis(150ms);
duration::mins(30m);
duration::nanos(200ns);
duration::secs(25s);
duration::weeks(7w);
duration::years(7y);

-- encoding::
encoding::base64::decode("");
encoding::base64::encode(<bytes>"");
encoding::cbor::decode(b"A163666F6F63626172");
encoding::cbor::encode({ foo: "bar" });
encoding::json::encode("hello");
encoding::json::decode('{"url":"https:\\/\\/example.com\\/path"}');

-- eval::
RETURN eval::surql("RETURN 1");
RETURN eval::gql("MATCH (n:person) RETURN n.name AS name ORDER BY name");

-- file::
file::get(f"test:/a.txt").?.to_string();
file::head(f"test:/a.txt");
file::exists(f"test:/a.txt");
file::put_if_not_exists(f"test:/a.txt", "abc");
file::put(f"test:/a.txt", "def");
file::copy_if_not_exists(f"test:/a.txt", "b.txt");
file::copy(f"test:/b.txt", "a.txt");
file::rename_if_not_exists(f"test:/a.txt", "c.txt");
file::rename(f"test:/b.txt", "c.txt");
file::delete(f"test:/c.txt");
file::list("test");
file::bucket(f"test:/b.txt");
file::key(f"test:/b.txt");

-- geo::
SELECT * FROM place WHERE geo::distance(location, (-0.1276, 51.5072)) < 1000000;
geo::hash::decode('gcpvhchdswz9');
geo::is_valid((-0.118092, 51.509865));

-- math::
math::acos(0.55);
math::acot(0.5);
SELECT category, math::sum(n) AS total FROM samp GROUP BY category ORDER BY category;
SELECT category, math::mean(n) AS avg FROM samp GROUP BY category ORDER BY category;
SELECT category, math::min(n) AS smallest FROM samp GROUP BY category ORDER BY category;
SELECT category, math::max(n) AS largest FROM samp GROUP BY category ORDER BY category;
SELECT category, math::stddev(n) AS spread FROM samp GROUP BY category ORDER BY category;
SELECT category, math::variance(n) AS variance FROM samp GROUP BY category ORDER BY category;
SELECT category, math::median(n) AS mid FROM samp GROUP BY category ORDER BY category;
math::asin(0.55);
math::atan(0.0);
math::ceil(101);
math::cos(0.0);
math::cot(0.5);

-- object::
DEFINE FIELD suits ON TABLE hand TYPE option<set<string>> VALUE <set>object::keys(cards);
object::from_entries([ [ 'a', 1 ], [ 'b', 2 ] ]);
object::is_empty({});
object::len({ a: 1, b: 2 });
object::values({ a: 1, b: 2 , c: 1});

-- parse::
parse::email::host("john.doe@example.com");
parse::email::user("john.doe@example.com");

-- rand::
UPDATE record SET active = true, score = rand::float(0.0, 100.0);
CREATE |record:10000| SET n = rand::int(1, 100), text = rand::string(50) RETURN NONE;
CREATE test:[rand::ulid()] SET created = time::now(), num = 1 RETURN NONE;
CREATE test:[rand::uuid::v7()] SET created = time::now(), num = 1 RETURN NONE;

-- record::
SELECT id FROM knows WHERE record::tb(out) = 'nonexistent' ORDER BY id;
record::exists(r"person:tobie");
record::id(r"person:tobie");
record::table(r"person:tobie");

-- schema::
schema::table::exists("person");

-- search::
SELECT search::offsets(1) AS title, search::score(1) AS score FROM b WHERE t @1@ 'Bar';
SELECT id, search::highlight('<em>', '</em>', 1) AS content FROM blog WHERE content @1@ 'he';
SELECT id,search::score(1) AS score FROM blog WHERE title @1@ 'animals';
search::analyze('ngram_ana', 'Fox');

-- sequence::
sequence::nextval('order_id');

-- set::
RETURN set::at(type::set([1, 2, 3]), 0);
RETURN set::add({,}, NONE);
RETURN set::contains({NONE, NULL}, NONE);
RETURN set::complement({1,2,3}, {1,2,3});
RETURN set::intersect({1,2,3}, {,});
RETURN set::filter(type::set([1, 2, 3, 4, 5]), 2);
RETURN set::find(type::set([1, 2, 3, 4, 5]), 3);
RETURN set::first(type::set([3, 1, 2]));
RETURN set::flatten(type::set([type::set([1, 2]), type::set([3, 4]), type::set([5, 6])]));
RETURN set::join(type::set([3, 1, 2]), ', ');
RETURN set::last(type::set([1, 3, 2]));
RETURN set::max(type::set([1, 3, 2]));
RETURN set::min(type::set([3, 1, 2]));
RETURN set::slice(type::set([1, 2, 3, 4, 5]), 1, 3);

-- string::
DEFINE FIELD label ON person TYPE string COMPUTED string::uppercase(name) PERMISSIONS FULL;
string::capitalize("");
string::concat();
string::contains("", "");
string::ends_with("", "");
string::html::encode("<#!DOCTYPE HTML><div onclick=\"foo()\">Hello world!</div>");
string::is_alphanum("12345");
string::is_alpha("abc");
string::is_ascii("abc");
string::is_domain("食狮.中国");
string::is_email("user@[fd79:cdcb:38cc:9dd:f686:e06d:32f3:c123]");
string::is_hexadecimal("00FF00");
string::is_ip("127.0.0.1");
string::is_ipv4("127.0.0.1");

-- time::
SELECT time::group(time, "day") AS day, count() FROM event GROUP BY day;
time::now().is_datetime();
SELECT time::min(time) AS t FROM tgt GROUP ALL;
SELECT *, time::year(time) AS year FROM temperature;
SELECT time::day(timestamp) as day, COUNT() as count FROM event GROUP BY time::day(timestamp);
SELECT VALUE time::max(a) FROM event7301 GROUP ALL;

-- type::
type::table("a") < a:b;
type::is_uuid(rand::uuid::v4());
RETURN type::set([1, 2, 3]);
RETURN type::is_set({1, 2, 3});
type::record("person", u'01958299-720d-7c33-9eb6-232ffb1874e5');
RETURN type::range(..);
RETURN (SELECT VALUE type::of(value) FROM test_values:none)[0];
CREATE test_values:file SET value = type::file("bucket", "key");
type::is_bytes(file::get(f"test:/data.bin"));
type::field('bob');
SELECT id, score FROM item WHERE score > type::int("25") ORDER BY score ASC LIMIT 3;

-- value::
RETURN value::diff({ a: 1, b: 2 }, { c: 3, b: 2 });


-- Composed calls, where one function's return type feeds another's argument.
-- Not verbatim from the corpus: written here because this is the surface the
-- return types opened. Each inner call's declared return type satisfies the
-- outer parameter, so every line must stay silent.
RETURN string::len(rand::string(5));
RETURN string::uppercase(crypto::sha256('tobie'));
RETURN time::day(time::now());
RETURN time::floor(time::now(), 1h);
RETURN duration::secs(rand::duration(1s, 2s));
RETURN duration::days(time::now() - time::now());
RETURN math::abs(array::len([1, 2]));
RETURN array::len(object::keys({ a: 1 }));
RETURN array::len(string::split('a,b', ','));
RETURN math::sum(vector::add([1, 2], [3, 4]));
RETURN string::concat(type::string(rand::int(1, 9)), 'x');
RETURN type::is_uuid(rand::uuid::v7(time::now()));
RETURN string::len(type::string(time::now()));
RETURN time::unix(rand::time(d'2024-01-01T00:00:00Z', d'2025-01-01T00:00:00Z'));
