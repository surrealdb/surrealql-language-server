-- Adversarial fixture for the type checker.
--
-- SYNTHESIZED, not copied from any real deployment. Every construct here
-- was chosen because it is something the checker could plausibly get
-- wrong, and most of them are things it *did* get wrong at some point:
--
--   * literal union types            TYPE 'Draft' | 'Open' | …
--   * array-literal (tuple) types    TYPE [string, string]
--   * a union inside record<>        record<order | project>
--   * inline multi-line object types as function parameters
--   * option<T> flowing into non-optional parameters
--   * optional-chained method idioms $value.?.trim()
--   * OR used as null-coalescing     array::distinct($value OR [])
--   * record ids built from subscripts  project:[$value[0], $value[1]]
--   * open-ended record id ranges    orderLine:[$id[0], NONE]..[$id[0], ..]
--   * empty array literal against array<record<user>>
--   * `.*` projection and $rows[0].id() method calls
--   * field access on a coalesced union  ($after OR $before).id
--   * LET bindings nested inside DEFINE EVENT … THEN { IF … { … } }
--   * expression-bodied closures     $xs.filter(|$x| $x != 'Done')
--   * SELECT VALUE … FROM ONLY, CREATE ONLY, UPSERT ONLY
--
-- The tests over this file assert that none of it produces a type,
-- argument, or undefined-variable diagnostic. It is the tripwire for
-- false positives: a checker that is too eager lights this file up.

DEFINE TABLE IF NOT EXISTS orderData SCHEMAFULL;

DEFINE FIELD OVERWRITE id ON orderData TYPE [string, string]
  ASSERT project:[$value[0], $value[1]].exists()
  COMMENT 'Composite so a project can be queried with a range scan';
DEFINE FIELD OVERWRITE status ON orderData TYPE 'Draft' | 'Open' | 'Awaiting Approval' | 'Approved' | 'Cancelled' DEFAULT ALWAYS 'Draft';
DEFINE FIELD OVERWRITE owner ON orderData TYPE option<string> VALUE $value.?.trim();
DEFINE FIELD OVERWRITE layout ON orderData TYPE option<record<layout>>;
DEFINE FIELD OVERWRITE plannedStartDate ON orderData TYPE option<datetime>;
DEFINE FIELD OVERWRITE plannedEndDate ON orderData TYPE option<datetime> ASSERT (!!plannedStartDate AND $value >= plannedStartDate) OR !$value;
DEFINE FIELD OVERWRITE actualStartDate ON orderData TYPE option<datetime>;
DEFINE FIELD OVERWRITE actualEndDate ON orderData TYPE option<datetime> ASSERT (!!actualStartDate AND $value >= actualStartDate AND progress = 100) OR !$value;
DEFINE FIELD OVERWRITE handler ON orderData TYPE option<record<user>>;
DEFINE FIELD OVERWRITE minimumWeight ON orderData TYPE option<decimal> ASSERT $value=NONE OR $value >= 0 DEFAULT ALWAYS 0;
DEFINE FIELD OVERWRITE minimumDepth ON orderData TYPE option<decimal> ASSERT $value=NONE OR $value >= 0 DEFAULT ALWAYS 0;
DEFINE FIELD OVERWRITE approval ON orderData TYPE option<record<approval>>;
DEFINE FIELD OVERWRITE assignees ON orderData TYPE array<record<user>> DEFAULT ALWAYS [];
DEFINE FIELD OVERWRITE units ON orderData TYPE option<record<unitConstants>>;
DEFINE FIELD OVERWRITE permissions ON orderData TYPE object DEFAULT ALWAYS { read: [], write: []};
DEFINE FIELD OVERWRITE permissions.read ON orderData TYPE array<record<user>> VALUE array::distinct($value OR []);
DEFINE FIELD OVERWRITE permissions.write ON orderData TYPE array<record<user>> VALUE array::distinct($value OR []);
DEFINE FIELD OVERWRITE name ON orderData TYPE string VALUE $value.?.trim() ASSERT ($value OR '').len() > 0 AND ($value OR '').len() <= 512;
DEFINE FIELD OVERWRITE reference ON orderData TYPE string VALUE $value.?.trim() ASSERT ($value OR '').len() > 0 AND ($value OR '').len() < 128;
DEFINE FIELD OVERWRITE address ON orderData TYPE option<record<address>>;
DEFINE FIELD OVERWRITE progress ON orderData TYPE option<int> DEFAULT ALWAYS 0 ASSERT $value >= 0 AND $value <= 100;
DEFINE FIELD OVERWRITE updatedBy ON orderData TYPE record<user>;
DEFINE FIELD OVERWRITE updatedAt ON orderData VALUE time::now();

DEFINE TABLE IF NOT EXISTS orderDataHistory SCHEMALESS;

DEFINE FIELD OVERWRITE id ON orderDataHistory TYPE [string, string] ASSERT $value[1].is_ulid()
  COMMENT 'Composite so a project can be queried with a range scan';

DEFINE EVENT OVERWRITE orderDataDelete ON TABLE orderData WHEN $event = 'DELETE' ASYNC THEN {
  THROW 'not supported yet';
  LET $id = $before.id;
  DELETE orderDoc:[$id[0], $id[1], NONE]..[$id[0], $id[1], ..];
  DELETE note WHERE reference = $id;
  DELETE orderDataHistory:[$id[1], NONE]..[$id[1], ..];
  DELETE orderLine:[$id[0], $id[1], NONE]..[$id[0], $id[1], ..];
};

DEFINE EVENT OVERWRITE orderDataUpdate ON TABLE orderData WHEN $event IN ['UPDATE','CREATE'] ASYNC THEN {
  LET $id = $after.id;
  CREATE orderDataHistory:[$id[1], rand::ulid()] CONTENT {
    snapshot: $after
  } RETURN NONE;
};

DEFINE TABLE IF NOT EXISTS orderLine SCHEMALESS;
DEFINE TABLE IF NOT EXISTS orderDoc SCHEMALESS;
DEFINE TABLE IF NOT EXISTS orderHistory SCHEMALESS;
DEFINE TABLE IF NOT EXISTS layout SCHEMALESS;
DEFINE TABLE IF NOT EXISTS unitConstants SCHEMALESS;
DEFINE TABLE IF NOT EXISTS approval SCHEMALESS;
DEFINE TABLE IF NOT EXISTS address SCHEMALESS;
DEFINE TABLE IF NOT EXISTS note SCHEMALESS;
DEFINE TABLE IF NOT EXISTS company SCHEMALESS;
DEFINE TABLE IF NOT EXISTS project SCHEMALESS;
DEFINE TABLE IF NOT EXISTS user SCHEMALESS;

DEFINE FUNCTION OVERWRITE fn::canUserView($user: record<user>, $target: record<orderData>) -> bool {
  RETURN $user IN $target.permissions.read OR $user IN $target.permissions.write;
};

DEFINE FUNCTION OVERWRITE fn::canUserEdit($user: record<user>, $target: record<orderData | project>) -> bool {
  RETURN $user IN $target.permissions.write;
};

DEFINE FUNCTION OVERWRITE fn::diff($before: object, $after: object) -> object {
  RETURN $after;
};

DEFINE FUNCTION OVERWRITE fn::orderData::getLineIDs($user: record<user>, $orderData: record<orderData>) {
  IF !fn::canUserView($user, $orderData) {
    THROW 'You do not have permission to view this order';
  };
  RETURN SELECT VALUE id FROM orderLine:[$orderData.id[1], NONE]..[$orderData.id[1], ..];
};

DEFINE FUNCTION OVERWRITE fn::orderData::get($user: record<user>, $orderData: record<orderData>) {
  IF !fn::canUserView($user, $orderData) {
    THROW 'You do not have permission to view this order';
  };

  LET $lines = SELECT VALUE completionStatus FROM orderLine:[$orderData.id[1], NONE]..[$orderData.id[1], ..];
  LET $company = SELECT id, name FROM ONLY type::record('company', $orderData.id[0]);

  RETURN SELECT *, $company AS company, $lines.len() AS totalLines, ($lines.filter(|$line| $line != 'Complete')).len() AS incompleteLines FROM ONLY $orderData
  FETCH address, approval, approval.approvedBy;
};

DEFINE FUNCTION OVERWRITE fn::orderData::getLines($user: record<user>, $orderData: record<orderData>) {
  IF !fn::canUserView($user, $orderData) {
    THROW 'You do not have permission to view this order';
  };
  LET $lines = SELECT * FROM orderLine:[$orderData.id[1], NONE]..[$orderData.id[1], ..];
  LET $company = SELECT id, name FROM ONLY type::record('company', $orderData.id[0]);
  RETURN $lines;
};

DEFINE FUNCTION OVERWRITE fn::orderData::getNotes($user: record<user>, $orderData: record<orderData>) {
  IF !fn::canUserView($user, $orderData) {
    THROW 'You do not have permission to view this order';
  };
  LET $notes = SELECT * FROM note WHERE reference = $orderData FETCH attachments, updatedBy;
  RETURN $notes;
};

DEFINE FUNCTION OVERWRITE fn::orderData::create($user: record<user>, $orderData: object) {
  LET $id = $orderData.project;
  IF !fn::canUserEdit($user, $id) {
    THROW 'You do not have permission to create an order here';
  };
  LET $created = UPSERT ONLY orderData:[$id[0], $id[1]] CONTENT { name: 'draft', updatedBy: $user } RETURN AFTER;
  RETURN $created;
};

DEFINE FUNCTION OVERWRITE fn::orderData::update($user: record<user>, $orderData: object, $address: option<object>) {
  LET $existing = SELECT * FROM ONLY $orderData.id;
  IF !fn::canUserEdit($user, $existing) {
    THROW 'You do not have permission to edit this order';
  };
  RETURN UPDATE ONLY $orderData.id SET updatedBy = $user RETURN AFTER;
};

DEFINE FUNCTION OVERWRITE fn::orderData::reopen($user: record<user>, $orderData: record<orderData>) {
  IF !fn::canUserEdit($user, $orderData) {
    THROW 'You do not have permission to reopen this order';
  };
  RETURN UPDATE ONLY $orderData SET status = 'Open' RETURN AFTER;
};

DEFINE FUNCTION OVERWRITE fn::orderData::getDocs($user: record<user>, $orderData: record<orderData>) {
  IF !fn::canUserView($user, $orderData) {
    THROW 'You do not have permission to view this order';
  };
  RETURN SELECT * FROM orderDoc:[$orderData.id[0], $orderData.id[1], NONE]..[$orderData.id[0], $orderData.id[1], ..];
};

DEFINE FUNCTION OVERWRITE fn::orderData::prepareToDelete($user: record<user>, $orderData: record<orderData>) {
  IF !fn::canUserEdit($user, $orderData) {
    THROW 'You do not have permission to delete this order';
  };
  LET $lines = SELECT VALUE id FROM orderLine:[$orderData.id[1], NONE]..[$orderData.id[1], ..];
  RETURN $lines;
};

DEFINE FUNCTION OVERWRITE fn::orderData::delete($user: record<user>, $orderData: record<orderData>) {
  IF !fn::canUserEdit($user, $orderData) {
    THROW 'You do not have permission to delete this order';
  };
  RETURN DELETE $orderData RETURN BEFORE;
};

DEFINE FUNCTION OVERWRITE fn::orderData::addLayout($user: record<user>, $orderData: record<orderData>, $image: record<asset>) {
  IF !fn::canUserEdit($user, $orderData) {
    THROW 'You do not have permission to edit this order';
  };
  LET $layout = CREATE ONLY layout:[$orderData.id[0], $orderData.id[1], rand::ulid()] CONTENT {
    image: $image,
    updatedBy: $user
  } RETURN AFTER;
  RETURN $layout;
};

DEFINE FUNCTION OVERWRITE fn::orderData::getProgress($user: record<user>, $orderData: record<orderData>) -> option<int> {
  IF !fn::canUserView($user, $orderData) {
    THROW 'You do not have permission to view this order';
  };
  RETURN $orderData.progress;
};

DEFINE FUNCTION OVERWRITE fn::orderData::export($user: record<user>, $orderData: record<orderData>) {
  IF !fn::canUserView($user, $orderData) {
    THROW 'You do not have permission to view this order';
  };
  LET $rows = SELECT * FROM orderLine:[$orderData.id[1], NONE]..[$orderData.id[1], ..];
  RETURN $rows;
};

DEFINE FUNCTION OVERWRITE fn::doc::add($user: record<user>, $doc: {
  line: record<orderLine>,
  asset: record<asset>
}) {
  IF !fn::canUserEdit($user, $doc.line) {
    THROW 'You do not have permission to edit this line';
  };
  LET $line = $doc.line.*;
  CREATE orderDoc:[$line.id[0], $line.id[1], rand::ulid()] CONTENT {
    asset: $doc.asset
  };

  RETURN SELECT *, $line AS line FROM orderDoc:[$line.id[0], $line.id[1], NONE]..[$line.id[0], $line.id[1], ..] FETCH asset;
};

DEFINE FUNCTION OVERWRITE fn::doc::delete($user: record<user>, $doc: record<orderDoc>) {
  IF !fn::canUserEdit($user, orderData:[$doc.line.id[0], $doc.line.id[1]]) {
    THROW 'You do not have permission to delete this document';
  };
  RETURN DELETE $doc RETURN BEFORE;
};

DEFINE FUNCTION OVERWRITE fn::line::get($user: record<user>, $line: record<orderLine>) {
  IF !fn::canUserView($user, orderData:[$line.id[0], $line.id[1]]) {
    THROW 'You do not have permission to view this line';
  };
  RETURN SELECT * FROM ONLY $line;
};

DEFINE FUNCTION OVERWRITE fn::line::getLines($user: record<user>, $lines: array<record<orderLine>>) {
  LET $first = $lines[0].id();
  RETURN SELECT * FROM $lines;
};

DEFINE FUNCTION OVERWRITE fn::line::getDocs($user: record<user>, $line: record<orderLine>) {
  RETURN SELECT * FROM orderDoc:[$line.id[0], $line.id[1], NONE]..[$line.id[0], $line.id[1], ..];
};

DEFINE FUNCTION OVERWRITE fn::line::update($user: record<user>, $line: object) {
  LET $existing = SELECT * FROM ONLY $line.id;
  IF !fn::canUserEdit($user, $existing) {
    THROW 'You do not have permission to edit this line';
  };
  RETURN UPDATE ONLY $line.id SET updatedBy = $user RETURN AFTER;
};

DEFINE FUNCTION OVERWRITE fn::line::updateLines($user: record<user>, $template: object, $lines: array<record<orderLine>>) {
  LET $newTemplate = $template.remove('id') + { updatedBy: $user };
  LET $id = $lines[0].id();
  RETURN UPDATE $lines SET updatedBy = $user RETURN AFTER;
};

DEFINE FUNCTION OVERWRITE fn::line::addLines($user: record<user>, $id: record<orderData | project>, $template: object, $amount: decimal) {
  IF !fn::canUserEdit($user, $id) {
    THROW 'You do not have permission to add lines here';
  };

  FOR $i IN 0..$amount {
    CREATE orderLine:[$id[1], rand::ulid()] CONTENT $template RETURN NONE;
  };

  RETURN SELECT * FROM orderLine:[$id[1], NONE]..[$id[1], ..];
};

DEFINE FUNCTION OVERWRITE fn::line::getHistory($user: record<user>, $line: record<orderLine>) {
  RETURN SELECT * FROM orderHistory:[$line.id[1], NONE]..[$line.id[1], ..];
};

DEFINE FUNCTION OVERWRITE fn::line::getNotes($user: record<user>, $line: record<orderLine>) {
  RETURN SELECT * FROM note WHERE reference = $line;
};

DEFINE FUNCTION OVERWRITE fn::line::delete($user: record<user>, $lines: array<record<orderLine>>) {
  LET $orderData = $lines[0].orderData;
  IF !fn::canUserEdit($user, $orderData) {
    THROW 'You do not have permission to delete these lines';
  };
  RETURN DELETE $lines RETURN BEFORE;
};

DEFINE FUNCTION OVERWRITE fn::address::update($user: record<user>, $address: object) {
  RETURN UPDATE ONLY $address.id SET updatedBy = $user RETURN AFTER;
};

DEFINE FUNCTION OVERWRITE fn::companyData::updateHandlers($user: record<user>, $company: record<company>, $handlers: array<record<user>>) {
  RETURN UPDATE ONLY $company SET handlers = array::distinct($handlers OR []) RETURN AFTER;
};

DEFINE EVENT OVERWRITE lineUpdate ON TABLE orderLine WHEN $event IN ['UPDATE','CREATE', 'DELETE'] ASYNC THEN {
  IF $event != 'DELETE' {
    LET $bef = $before.?.remove(['updatedAt']);
    LET $aft = $after.?.remove(['updatedAt']);
    LET $id = ($after OR $before).id;
    IF $bef != $aft {
      LET $updatedBy = SELECT id, name, email FROM ONLY $after.updatedBy;
      CREATE orderHistory:[$id[1], rand::ulid()] CONTENT { changedBy: $updatedBy } RETURN NONE;
      UPDATE $after.orderData SET updatedBy = $after.updatedBy RETURN NONE;
    };
  };
};
