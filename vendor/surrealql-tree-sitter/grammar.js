/**
 * SurrealQL tree-sitter grammar.
 *
 * This grammar began as a 1:1 port of lezer-surrealql, and its rule names and
 * node shapes still come from there — see the naming conventions below, which
 * are why the rules are PascalCase rather than snake_case.
 *
 * What it tracks, though, is the SurrealDB engine: the shapes `surrealdb-core`
 * accepts, and the trees its `BindingPower` implies. Where lezer and the engine
 * disagree, the engine wins. The divergences that already exist are deliberate,
 * each one checked against a live server before it was written, and each one
 * pinned by a corpus case — so do not treat a difference from lezer as a bug to
 * be closed. Check the engine first.
 *
 * Naming conventions (intentionally NOT snake_case):
 *   - Visible rules use the same PascalCase names as lezer node types.
 *   - Hidden rules (lowercase passthrough in lezer) use a leading underscore.
 *   - Token-as-node visibility (Keyword, Operator, BraceOpen, …) is achieved by
 *     defining a visible rule with the lezer name and aliasing source tokens
 *     into it at each use site via alias($._tok, $.Name).
 *
 * Reference: codemirror/packages/lezer-surrealql/src/surrealql.grammar
 */

/// <reference types="tree-sitter-cli/dsl" />
// @ts-check

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/**
 * Case-insensitive keyword regex like /[sS][eE][lL][eE][cC][tT]/. Returned
 * directly (without `token(prec(...))`) so that tree-sitter's lexer applies
 * its default longest-match rules across keyword variants. Identifier vs.
 * keyword disambiguation is handled per-rule via static precedence on
 * `_rawident`.
 * @param {string} word
 */
function kw(word) {
	return new RegExp(
		[...word].map((c) => `[${c.toLowerCase()}${c.toUpperCase()}]`).join(''),
	);
}

/** Comma separated list (no trailing comma) */
function csep(rule) {
	return seq(rule, repeat(seq(',', rule)));
}

/** Comma separated with optional trailing comma */
function csepTrail(rule) {
	return seq(rule, repeat(seq(',', rule)), optional(','));
}

/** Pipe separated */
function piped(rule) {
	return seq(rule, repeat(seq('|', rule)));
}

/** Digit sequence with optional underscore separators (e.g. 1_000_000) */
const DIGITS = /[0-9]+(?:_[0-9]+)*/;

// ---------------------------------------------------------------------------
// Grammar
// ---------------------------------------------------------------------------

export default grammar({
	name: 'surrealql',

	word: ($) => $._rawident,

	extras: ($) => [/\s/, $.Comment, $.BlockComment],

	externals: ($) => [
		$._js_function_body,
		$._object_open, // emitted when '{' starts an Object (vs Block/Set)
	],

	precedences: ($) => [
		[
			'prefix',
			'range',
			'method',
			// Binary operator tiers, tightest to loosest. Splitting the former
			// single 'binary' level is what makes `a > 1 AND b > 2` parse as
			// `(a > 1) AND (b > 2)` instead of flat-left. The ordering mirrors
			// surrealdb-core's `BindingPower` enum
			// (Nullish < Or < And < Equality < Relation < AddSub < MulDiv <
			// Power), so the nesting matches how the engine evaluates.
			'binary_power',
			'binary_multiplicative',
			'binary_additive',
			'binary_relation',
			'binary_equality',
			'binary_conjunction',
			'binary_disjunction',
			'binary_nullish',
			'closure',
			'union',
			'filter',
			'for',
			'clause',
		],
	],

	conflicts: ($) => [
		[$.RecordId, $.RecordIdRange],
		[$.Path],
		[$.Path, $.Destructure],
		[$.WhereClause],
		[$._baseValue, $.Closure],
		[$._idName, $._singleType],
		[$.Legacy, $._baseValue],
		[$._prefixOperand, $.Path],
		[$._value, $.Path],
		// `-5` is a signed literal and `-$x` a prefix negation; both start the
		// same way, and the signed literal wins (dynamic precedence on
		// `Number`) whenever the operand is a bare number.
		[$.Number],
	],

	rules: {
		// ================================================================
		// Top-level entry
		// ================================================================

		SurrealQL: ($) => optional($._expressions),

		_expressions: ($) =>
			prec.right(
				seq(
					$._expression,
					repeat(seq(';', $._expression)),
					optional(';'),
				),
			),

		_expression: ($) => choice($._statement, $._value),

		// ================================================================
		// Statements
		// ================================================================

		_subqueryStatement: ($) =>
			choice(
				$.IfElseStatement,
				$.LetStatement,
				$.DeleteStatement,
				$.CreateStatement,
				$.SelectStatement,
				$.RelateStatement,
				$.UpdateStatement,
				$.RemoveStatement,
				$.UpsertStatement,
				$.ReturnStatement,
				$.AlterStatement,
				$.DefineStatement,
				$.RebuildStatement,
				$.InsertStatement,
			),

		_statement: ($) =>
			choice(
				$.BeginStatement,
				$.CancelStatement,
				$.CommitStatement,
				$.InfoForStatement,
				$.KillStatement,
				$.LiveSelectStatement,
				$.ShowStatement,
				$.SleepStatement,
				$.UseStatement,
				$.OptionStatement,
				$.BreakStatement,
				$.ContinueStatement,
				$.ForStatement,
				$.ThrowStatement,
				$._subqueryStatement,
			),

		// ----------------------------------------------------------------
		// Transaction statements
		// ----------------------------------------------------------------

		BeginStatement: ($) =>
			seq(
				alias($._kw_begin, $.Keyword),
				optional(alias($._kw_transaction, $.Keyword)),
			),

		CancelStatement: ($) =>
			seq(
				alias($._kw_cancel, $.Keyword),
				optional(alias($._kw_transaction, $.Keyword)),
			),

		CommitStatement: ($) =>
			seq(
				alias($._kw_commit, $.Keyword),
				optional(alias($._kw_transaction, $.Keyword)),
			),

		// ----------------------------------------------------------------
		// Simple statements
		// ----------------------------------------------------------------

		BreakStatement: ($) => alias($._kw_break, $.Keyword),
		ContinueStatement: ($) => alias($._kw_continue, $.Keyword),
		SleepStatement: ($) => seq(alias($._kw_sleep, $.Keyword), $.Duration),
		ThrowStatement: ($) => seq(alias($._kw_throw, $.Keyword), $._value),
		ReturnStatement: ($) =>
			seq(alias($._kw_return, $.Keyword), $._expression),

		OptionStatement: ($) =>
			seq(
				alias($._kw_option, $.Keyword),
				$.Ident,
				optional(
					seq(
						'=',
						choice(
							alias($._kw_true, $.Bool),
							alias($._kw_false, $.Bool),
						),
					),
				),
			),

		KillStatement: ($) => seq(alias($._kw_kill, $.Keyword), $.String),

		// USE
		UseStatement: ($) =>
			seq(
				alias($._kw_use, $.Keyword),
				choice($._useNs, $._useDb, seq($._useNs, $._useDb)),
			),
		_useNs: ($) =>
			seq(
				choice(
					alias($._kw_ns, $.Keyword),
					alias($._kw_namespace, $.Keyword),
				),
				$.Ident,
			),
		_useDb: ($) =>
			seq(
				choice(
					alias($._kw_db, $.Keyword),
					alias($._kw_database, $.Keyword),
				),
				$.Ident,
			),

		// SHOW
		ShowStatement: ($) =>
			seq(
				alias($._kw_show, $.Keyword),
				alias($._kw_changes, $.Keyword),
				alias($._kw_for, $.Keyword),
				alias($._kw_table, $.Keyword),
				$.Ident,
				optional(
					seq(
						alias($._kw_since, $.Keyword),
						// SINCE accepts a datetime string or a versionstamp number.
						choice($.String, $.Number),
					),
				),
				optional(seq(alias($._kw_limit, $.Keyword), $.Number)),
			),

		// INFO FOR
		InfoForStatement: ($) =>
			seq(
				alias($._kw_info, $.Keyword),
				alias($._kw_for, $.Keyword),
				choice(
					alias($._kw_root, $.Keyword),
					alias($._kw_ns, $.Keyword),
					alias($._kw_namespace, $.Keyword),
					alias($._kw_db, $.Keyword),
					alias($._kw_database, $.Keyword),
					seq(alias($._kw_sc, $.Keyword), $.Ident),
					seq(alias($._kw_scope, $.Keyword), $.Ident),
					seq(alias($._kw_tb, $.Keyword), $.Ident),
					seq(alias($._kw_table, $.Keyword), $.Ident),
				),
				optional(alias($._kw_structure, $.Keyword)),
			),

		// LET
		LetStatement: ($) =>
			seq(
				alias($._kw_let, $.Keyword),
				alias($._unionParamDefinition, $.ParamDefinition),
				'=',
				choice($._value, $._subqueryStatement),
			),

		// REBUILD
		RebuildStatement: ($) =>
			seq(
				alias($._kw_rebuild, $.Keyword),
				alias($._kw_index, $.Keyword),
				optional($.IfExistsClause),
				$.Ident,
				$.OnTableClause,
			),

		// FOR
		ForStatement: ($) =>
			seq(
				alias($._kw_for, $.Keyword),
				$.VariableName,
				alias($._kw_in, $.Keyword),
				choice(
					$.Array,
					$.VariableName,
					$.Range,
					$.SubQuery,
					$._subqueryStatement,
					$.Block,
				),
				$.Block,
			),

		// IF/ELSE
		IfElseStatement: ($) =>
			seq(alias($._kw_if, $.Keyword), choice($.Legacy, $.Modern)),
		Legacy: ($) =>
			seq(
				$._value,
				alias($._kw_then, $.Keyword),
				choice($.Block, $.SubQuery, $._value),
				repeat(
					seq(
						alias($._kw_else, $.Keyword),
						alias($._kw_if, $.Keyword),
						$._value,
						alias($._kw_then, $.Keyword),
						choice($.Block, $.SubQuery, $._value),
					),
				),
				optional(
					seq(
						alias($._kw_else, $.Keyword),
						choice($.Block, $.SubQuery, $._value),
					),
				),
				alias($._kw_end, $.Keyword),
			),
		Modern: ($) =>
			seq(
				$._value,
				$.Block,
				repeat(
					seq(
						alias($._kw_else, $.Keyword),
						alias($._kw_if, $.Keyword),
						$._value,
						$.Block,
					),
				),
				optional(seq(alias($._kw_else, $.Keyword), $.Block)),
			),

		// LIVE SELECT
		LiveSelectStatement: ($) =>
			seq(
				alias($._kw_live, $.Keyword),
				alias($._kw_select, $.Keyword),
				choice(
					alias($._kw_diff, $.Literal),
					seq(alias($._kw_value, $.Keyword), $.Predicate),
					csep($._inclusivePredicate),
				),
				alias($._kw_from, $.Keyword),
				csep(choice($.Ident, $.RecordId)),
				optional($.WhereClause),
				optional($.FetchClause),
			),

		// ALTER
		AlterStatement: ($) =>
			seq(
				alias($._kw_alter, $.Keyword),
				alias($._kw_table, $.Keyword),
				optional($.IfNotExistsClause),
				$._value,
				repeat(
					choice(
						alias($._kw_drop, $.Keyword),
						alias($._kw_schemafull, $.Keyword),
						alias($._kw_schemaless, $.Keyword),
						$.PermissionsForClause,
						$.CommentClause,
					),
				),
			),

		// REMOVE
		RemoveStatement: ($) =>
			seq(
				alias($._kw_remove, $.Keyword),
				choice(
					seq(
						alias($._kw_namespace, $.Keyword),
						optional($.IfExistsClause),
						$._value,
						optional(
							seq(
								alias($._kw_and, $.Keyword),
								alias($._kw_expunge, $.Keyword),
							),
						),
					),
					seq(
						alias($._kw_database, $.Keyword),
						optional($.IfExistsClause),
						$._value,
						optional(
							seq(
								alias($._kw_and, $.Keyword),
								alias($._kw_expunge, $.Keyword),
							),
						),
					),
					seq(
						alias($._kw_user, $.Keyword),
						optional($.IfExistsClause),
						$._value,
						alias($._kw_on, $.Keyword),
						choice(
							alias($._kw_root, $.Keyword),
							alias($._kw_namespace, $.Keyword),
							alias($._kw_database, $.Keyword),
						),
					),
					seq(
						alias($._kw_token, $.Keyword),
						optional($.IfExistsClause),
						$._value,
						alias($._kw_on, $.Keyword),
						choice(
							alias($._kw_namespace, $.Keyword),
							alias($._kw_database, $.Keyword),
							alias($._kw_scope, $.Keyword),
						),
					),
					seq(
						alias($._kw_event, $.Keyword),
						optional($.IfExistsClause),
						$._value,
						$.OnTableClause,
					),
					seq(
						alias($._kw_field, $.Keyword),
						optional($.IfExistsClause),
						$._value,
						$.OnTableClause,
					),
					seq(
						alias($._kw_index, $.Keyword),
						optional($.IfExistsClause),
						$._value,
						$.OnTableClause,
					),
					seq(
						alias($._kw_analyzer, $.Keyword),
						optional($.IfExistsClause),
						$._value,
					),
					seq(
						alias($._kw_function, $.Keyword),
						optional($.IfExistsClause),
						$.FunctionName,
					),
					seq(
						alias($._kw_param, $.Keyword),
						optional($.IfExistsClause),
						$.VariableName,
					),
					seq(
						alias($._kw_scope, $.Keyword),
						optional($.IfExistsClause),
						$._value,
					),
					seq(
						alias($._kw_table, $.Keyword),
						optional($.IfExistsClause),
						$._value,
						optional(
							seq(
								alias($._kw_and, $.Keyword),
								alias($._kw_expunge, $.Keyword),
							),
						),
					),
					seq(
						alias($._kw_api, $.Keyword),
						optional($.IfExistsClause),
						$._value,
					),
					seq(
						alias($._kw_bucket, $.Keyword),
						optional($.IfExistsClause),
						$._value,
					),
				),
			),

		// DEFINE
		DefineStatement: ($) =>
			seq(
				alias($._kw_define, $.Keyword),
				choice(
					$.AccessDefinition,
					seq(
						alias($._kw_namespace, $.Keyword),
						$._defineNamespaceOptions,
					),
					seq(
						alias($._kw_database, $.Keyword),
						$._defineDatabaseOptions,
					),
					seq(alias($._kw_user, $.Keyword), $._defineUserOptions),
					seq(alias($._kw_token, $.Keyword), $._defineTokenOptions),
					seq(alias($._kw_event, $.Keyword), $._defineEventOptions),
					seq(alias($._kw_field, $.Keyword), $._defineFieldOptions),
					seq(alias($._kw_index, $.Keyword), $._defineIndexOptions),
					seq(
						alias($._kw_analyzer, $.Keyword),
						$._defineAnalyzerOptions,
					),
					seq(
						alias($._kw_function, $.Keyword),
						$._defineFunctionOptions,
					),
					seq(alias($._kw_param, $.Keyword), $._defineParamOptions),
					$.ScopeDefinition,
					seq(alias($._kw_table, $.Keyword), $._defineTableOptions),
					seq(alias($._kw_config, $.Keyword), $._defineConfigOptions),
					seq(alias($._kw_api, $.Keyword), $._defineApiOptions),
					seq(alias($._kw_bucket, $.Keyword), $._defineBucketOptions),
				),
			),
		AccessDefinition: ($) =>
			seq(alias($._kw_access, $.Keyword), $._defineAccessOptions),
		ScopeDefinition: ($) =>
			seq(alias($._kw_scope, $.Keyword), $._defineScopeOptions),

		_defineAccessOptions: ($) =>
			seq(
				optional(choice($.IfNotExistsClause, $.OverwriteClause)),
				$._value,
				$.OnRootNsDbClause,
				repeat(
					choice(
						$.AccessTypeClause,
						$.AuthenticateClause,
						$.DurationClause,
						$.CommentClause,
					),
				),
			),

		_defineAnalyzerOptions: ($) =>
			seq(
				optional(choice($.IfNotExistsClause, $.OverwriteClause)),
				$._value,
				repeat(
					choice(
						$.TokenizersClause,
						$.FiltersClause,
						$.FunctionClause,
						$.CommentClause,
					),
				),
			),

		_defineEventOptions: ($) =>
			seq(
				optional(choice($.IfNotExistsClause, $.OverwriteClause)),
				$._value,
				$.OnTableClause,
				repeat(choice($.WhenClause, $.ThenClause, $.CommentClause)),
			),

		_defineDatabaseOptions: ($) =>
			seq(
				optional(choice($.IfNotExistsClause, $.OverwriteClause)),
				$._value,
				optional(alias($._kw_strict, $.Keyword)),
				optional($.CommentClause),
			),

		_defineFieldOptions: ($) =>
			seq(
				optional(choice($.IfNotExistsClause, $.OverwriteClause)),
				$.Idiom,
				$.OnTableClause,
				repeat(
					choice(
						$.TypeClause,
						$.DefaultClause,
						$.ReadonlyClause,
						$.ValueClause,
						$.AssertClause,
						$.PermissionsForClause,
						$.CommentClause,
						$.ReferenceClause,
						$.ComputedClause,
					),
				),
			),

		_defineFunctionOptions: ($) =>
			seq(
				optional(choice($.IfNotExistsClause, $.OverwriteClause)),
				$.FunctionName, // customFunctionName aliased to FunctionName
				seq(
					'(',
					optional(
						csepTrail(
							alias($._unionParamDefinition, $.ParamDefinition),
						),
					),
					')',
				),
				optional(seq($.LookupRight, $._type)),
				$.Block,
				repeat(choice($.PermissionsBasicClause, $.CommentClause)),
			),

		_defineIndexOptions: ($) =>
			seq(
				optional(choice($.IfNotExistsClause, $.OverwriteClause)),
				$._value,
				$.OnTableClause,
				repeat(
					choice(
						$.FieldsColumnsClause,
						$.IndexClause,
						$.CommentClause,
						$.ConcurrentlyClause,
						$.DeferClause,
					),
				),
			),
		ConcurrentlyClause: ($) => alias($._kw_concurrently, $.Keyword),
		DeferClause: ($) => alias($._kw_defer, $.Keyword),

		_defineNamespaceOptions: ($) =>
			seq(
				optional(choice($.IfNotExistsClause, $.OverwriteClause)),
				$._value,
				optional($.CommentClause),
			),

		_defineParamOptions: ($) =>
			seq(
				optional(choice($.IfNotExistsClause, $.OverwriteClause)),
				$.VariableName,
				alias($._kw_value, $.Keyword),
				$._value,
				repeat(choice($.PermissionsBasicClause, $.CommentClause)),
			),

		_defineScopeOptions: ($) =>
			seq(
				optional(choice($.IfNotExistsClause, $.OverwriteClause)),
				$._value,
				repeat(
					choice(
						$.SessionClause,
						$.SigninClause,
						$.SignupClause,
						$.CommentClause,
					),
				),
			),

		_defineTableOptions: ($) =>
			seq(
				optional(choice($.IfNotExistsClause, $.OverwriteClause)),
				$._value,
				repeat(
					choice(
						alias($._kw_drop, $.Keyword),
						alias($._kw_schemafull, $.Keyword),
						alias($._kw_schemaless, $.Keyword),
						$.TableTypeClause,
						$.TableViewClause,
						$.ChangefeedClause,
						$.PermissionsForClause,
						$.CommentClause,
					),
				),
			),

		_defineConfigOptions: ($) =>
			seq(
				optional(choice($.IfNotExistsClause, $.OverwriteClause)),
				choice(
					seq(
						alias($._kw_graphql, $.Keyword),
						$._defineConfigGraphqlOptions,
					),
					seq(alias($._kw_api, $.Keyword), $.ApiOptions),
				),
			),
		_defineConfigGraphqlOptions: ($) =>
			repeat1(
				choice(
					alias($._kw_none, $.None),
					alias($._kw_auto, $.Keyword),
					seq(
						alias($._kw_tables, $.Keyword),
						choice(
							alias($._kw_none, $.None),
							alias($._kw_auto, $.Keyword),
							seq(alias($._kw_include, $.Keyword), csep($.Ident)),
							seq(alias($._kw_exclude, $.Keyword), csep($.Ident)),
						),
					),
					seq(
						alias($._kw_functions, $.Keyword),
						choice(
							alias($._kw_none, $.None),
							alias($._kw_auto, $.Keyword),
						),
					),
				),
			),

		_defineTokenOptions: ($) =>
			seq(
				optional(choice($.IfNotExistsClause, $.OverwriteClause)),
				$._value,
				seq(
					alias($._kw_on, $.Keyword),
					choice(
						alias($._kw_namespace, $.Keyword),
						alias($._kw_database, $.Keyword),
						seq(alias($._kw_scope, $.Keyword), $._value),
					),
				),
				$.TokenTypeClause,
				seq(alias($._kw_value, $.Keyword), $.String),
			),

		_defineUserOptions: ($) =>
			seq(
				optional(choice($.IfNotExistsClause, $.OverwriteClause)),
				$._value,
				$.OnRootNsDbClause,
				seq(
					choice(
						alias($._kw_password, $.Keyword),
						alias($._kw_passhash, $.Keyword),
					),
					$.String,
				),
				seq(alias($._kw_roles, $.Keyword), csep($.Ident)),
				optional($.DurationClause),
			),

		_defineApiOptions: ($) =>
			seq(
				optional(choice($.IfNotExistsClause, $.OverwriteClause)),
				$.String,
				optional($.ApiOptions),
				repeat1(
					seq(
						alias($._kw_for, $.Keyword),
						choice(alias($._kw_any, $.Keyword), csep($.HttpMethod)),
						optional($.ApiOptions),
						alias($._kw_then, $.Keyword),
						$.Block,
					),
				),
			),

		ApiOptions: ($) =>
			repeat1(choice($.PermissionsBasicClause, $.MiddlewareClause)),

		_defineBucketOptions: ($) =>
			seq(
				optional(choice($.IfNotExistsClause, $.OverwriteClause)),
				$._value,
				repeat(
					choice(
						$.BackendClause,
						$.PermissionsBasicClause,
						$.CommentClause,
					),
				),
			),

		// CREATE
		CreateStatement: ($) =>
			seq(
				alias($._kw_create, $.Keyword),
				optional(alias($._kw_only, $.Keyword)),
				csep(
					choice(
						$.Ident,
						$.VariableName,
						$.FunctionCall,
						$.RecordId,
						$.RangeRecordId,
					),
				),
				optional(choice($.ContentClause, $.SetClause, $.UnsetClause)),
				optional($.ReturnClause),
				optional($.TimeoutClause),
				optional($.ParallelClause),
			),

		// SELECT
		SelectStatement: ($) =>
			seq(
				alias($._kw_select, $.Keyword),
				$.Fields,
				optional($.OmitClause),
				alias($._kw_from, $.Keyword),
				optional(alias($._kw_only, $.Keyword)),
				choice(
					$._statement,
					seq(csep($._value), repeat($._modifierClause)),
				),
			),

		// DELETE
		DeleteStatement: ($) =>
			seq(
				alias($._kw_delete, $.Keyword),
				optional(alias($._kw_only, $.Keyword)),
				choice(
					$._statement,
					seq(
						csep($._value),
						repeat(
							choice(
								$.WhereClause,
								$.ReturnClause,
								$.TimeoutClause,
								$.ParallelClause,
							),
						),
					),
				),
			),

		// INSERT
		InsertStatement: ($) =>
			seq(
				alias($._kw_insert, $.Keyword),
				optional(alias($._kw_ignore, $.Keyword)),
				optional(alias($._kw_relation, $.Keyword)),
				optional(seq(alias($._kw_into, $.Keyword), $.Ident)),
				choice(
					$.Object,
					$.VariableName,
					$.BulkInsert,
					seq(
						'(',
						csep($.Ident),
						')',
						alias($._kw_values, $.Keyword),
						csep(seq('(', csep($._value), ')')),
					),
				),
				optional(
					seq(
						alias($._kw_on, $.Keyword),
						alias($._kw_duplicate, $.Keyword),
						alias($._kw_key, $.Keyword),
						alias($._kw_update, $.Keyword),
						csep($.FieldAssignment),
					),
				),
				optional($.ReturnClause),
			),
		BulkInsert: ($) => seq('[', csep($.Object), ']'),

		// UPDATE
		UpdateStatement: ($) =>
			seq(
				alias($._kw_update, $.Keyword),
				optional(alias($._kw_only, $.Keyword)),
				choice(
					$._statement,
					seq(
						csep($._value),
						optional($._dataClause),
						optional($.WhereClause),
						optional($.ReturnClause),
						optional($.TimeoutClause),
						optional($.ParallelClause),
					),
				),
			),

		// UPSERT
		UpsertStatement: ($) =>
			seq(
				alias($._kw_upsert, $.Keyword),
				optional(alias($._kw_only, $.Keyword)),
				choice(
					$._statement,
					seq(
						csep($._value),
						optional($._dataClause),
						optional($.WhereClause),
						optional($.ReturnClause),
						optional($.TimeoutClause),
						optional($.ParallelClause),
					),
				),
			),

		// RELATE
		_relateSubject: ($) =>
			choice(
				$.Array,
				$.Ident,
				$.FunctionCall,
				$.VariableName,
				$.RecordId,
			),
		RelateStatement: ($) =>
			seq(
				alias($._kw_relate, $.Keyword),
				optional(alias($._kw_only, $.Keyword)),
				$._relateSubject,
				choice($.LookupRight, $.LookupLeft),
				$._relateSubject,
				choice($.LookupRight, $.LookupLeft),
				$._relateSubject,
				optional(choice($.ContentClause, $.SetClause)),
				optional($.ReturnClause),
				optional($.TimeoutClause),
				optional($.ParallelClause),
			),

		// ----------------------------------------------------------------
		// Modifier / data clauses
		// ----------------------------------------------------------------

		_modifierClause: ($) =>
			choice(
				$.WithClause,
				$.WhereClause,
				$.SplitClause,
				$.GroupClause,
				$.OrderClause,
				$.LimitStartComboClause,
				$.FetchClause,
				$.TimeoutClause,
				$.ParallelClause,
				$.TempfilesClause,
				$.ExplainClause,
				$.VersionClause,
				$.ReturnClause,
			),

		_dataClause: ($) =>
			choice(
				$.ContentClause,
				$.SetClause,
				$.MergeClause,
				$.PatchClause,
				$.ReplaceClause,
				$.UnsetClause,
			),

		ContentClause: ($) => seq(alias($._kw_content, $.Keyword), $._value),
		SetClause: ($) =>
			seq(alias($._kw_set, $.Keyword), csep($.FieldAssignment)),
		MergeClause: ($) => seq(alias($._kw_merge, $.Keyword), $._value),
		PatchClause: ($) => seq(alias($._kw_patch, $.Keyword), $.Array),
		ReplaceClause: ($) => seq(alias($._kw_replace, $.Keyword), $.Object),
		// UNSET removes fields by name (`UNSET a, b`), so it takes a field
		// list — the same shape as OMIT — rather than assignments.
		UnsetClause: ($) =>
			seq(alias($._kw_unset, $.Keyword), csep($._inclusivePredicate)),
		OmitClause: ($) =>
			seq(alias($._kw_omit, $.Keyword), csep($._inclusivePredicate)),

		WhereClause: ($) =>
			seq(alias($._kw_where, $.Keyword), optional($._value)),

		WithClause: ($) =>
			seq(
				alias($._kw_with, $.Keyword),
				choice(
					alias($._kw_noindex, $.Keyword),
					seq(alias($._kw_index, $.Keyword), csep($.Ident)),
				),
			),

		SplitClause: ($) =>
			seq(
				alias($._kw_split, $.Keyword),
				optional(alias($._kw_on, $.Keyword)),
				$.Idiom,
			),

		GroupClause: ($) =>
			seq(
				alias($._kw_group, $.Keyword),
				choice(
					seq(optional(alias($._kw_by, $.Keyword)), csep($.Idiom)),
					alias($._kw_all, $.Keyword),
				),
			),

		OrderClause: ($) =>
			seq(
				alias($._kw_order, $.Keyword),
				optional(alias($._kw_by, $.Keyword)),
				choice(csep($.Order), $.FunctionCall),
			),
		Order: ($) =>
			seq(
				$.Idiom,
				optional(alias($._kw_collate, $.Keyword)),
				optional(alias($._kw_numeric, $.Keyword)),
				optional(
					choice(
						alias($._kw_asc, $.Keyword),
						alias($._kw_desc, $.Keyword),
					),
				),
			),

		LimitStartComboClause: ($) =>
			prec.right(
				choice(
					seq($.StartClause, optional($.LimitClause)),
					seq($.LimitClause, optional($.StartClause)),
				),
			),
		StartClause: ($) =>
			seq(
				alias($._kw_start, $.Keyword),
				optional(alias($._kw_at, $.Keyword)),
				choice($.Number, $.VariableName),
			),
		LimitClause: ($) =>
			seq(
				alias($._kw_limit, $.Keyword),
				optional(alias($._kw_by, $.Keyword)),
				choice($.Number, $.VariableName),
			),

		FetchClause: ($) => seq(alias($._kw_fetch, $.Keyword), csep($.Idiom)),
		TimeoutClause: ($) => seq(alias($._kw_timeout, $.Keyword), $.Duration),
		ParallelClause: ($) => alias($._kw_parallel, $.Keyword),
		TempfilesClause: ($) => alias($._kw_tempfiles, $.Keyword),
		ExplainClause: ($) =>
			seq(
				alias($._kw_explain, $.Keyword),
				optional(alias($._kw_full, $.Literal)),
			),
		VersionClause: ($) => seq(alias($._kw_version, $.Keyword), $.String),

		ReturnClause: ($) =>
			seq(
				alias($._kw_return, $.Keyword),
				choice(
					alias($._kw_before, $.Literal),
					alias($._kw_after, $.Literal),
					alias($._kw_diff, $.Literal),
					$.Fields,
				),
			),

		// ----------------------------------------------------------------
		// Other clauses
		// ----------------------------------------------------------------

		IfNotExistsClause: ($) =>
			seq(
				alias($._kw_if, $.Keyword),
				alias($._kw_not, $.Keyword),
				alias($._kw_exists, $.Keyword),
			),
		IfExistsClause: ($) =>
			seq(alias($._kw_if, $.Keyword), alias($._kw_exists, $.Keyword)),
		OverwriteClause: ($) => alias($._kw_overwrite, $.Keyword),

		OnTableClause: ($) =>
			seq(
				alias($._kw_on, $.Keyword),
				optional(alias($._kw_table, $.Keyword)),
				$._value,
			),

		OnRootNsDbClause: ($) =>
			seq(
				alias($._kw_on, $.Keyword),
				choice(
					alias($._kw_root, $.Keyword),
					alias($._kw_namespace, $.Keyword),
					alias($._kw_database, $.Keyword),
				),
			),

		AccessTypeClause: ($) =>
			seq(
				alias($._kw_type, $.Keyword),
				choice(
					seq(alias($._kw_jwt, $.Keyword), $.JwtClause),
					seq(
						alias($._kw_record, $.Keyword),
						repeat(choice($.SignupClause, $.SigninClause)),
						optional(
							seq(
								alias($._kw_with, $.Keyword),
								alias($._kw_jwt, $.Keyword),
								$.JwtClause,
								optional(
									seq(
										alias($._kw_with, $.Keyword),
										alias($._kw_issuer, $.Keyword),
										alias($._kw_key, $.Keyword),
										$.Ident,
									),
								),
							),
						),
					),
				),
			),

		JwtClause: ($) =>
			choice(
				seq(
					alias($._kw_algorithm, $.Keyword),
					$.Ident,
					alias($._kw_key, $.Keyword),
					$.Ident,
				),
				seq(alias($._kw_url, $.Keyword), $.String),
			),

		SignupClause: ($) =>
			seq(alias($._kw_signup, $.Keyword), choice($.SubQuery, $.Block)),
		SigninClause: ($) =>
			seq(alias($._kw_signin, $.Keyword), choice($.SubQuery, $.Block)),
		AuthenticateClause: ($) =>
			seq(
				alias($._kw_authenticate, $.Keyword),
				choice($.SubQuery, $.Block),
			),
		SessionClause: ($) => seq(alias($._kw_session, $.Keyword), $.Duration),

		DurationClause: ($) =>
			seq(
				alias($._kw_duration, $.Keyword),
				optional(
					seq(
						alias($._kw_for, $.Keyword),
						alias($._kw_session, $.Keyword),
					),
				),
				csep($.DurationValue),
			),
		DurationValue: ($) =>
			choice(
				seq(
					alias($._kw_for, $.Keyword),
					alias($._kw_token, $.Keyword),
					$.Duration,
				),
				seq(
					alias($._kw_for, $.Keyword),
					alias($._kw_session, $.Keyword),
					$.Duration,
				),
			),

		TokenTypeClause: ($) => seq(alias($._kw_type, $.Keyword), $.TokenType),

		FieldsColumnsClause: ($) =>
			seq(
				choice(
					alias($._kw_fields, $.Keyword),
					alias($._kw_columns, $.Keyword),
				),
				csep($.Idiom),
			),

		// The index kinds. SurrealDB 3 reads `UNIQUE`, `COUNT`, `FULLTEXT`,
		// `HNSW` and `DISKANN` (surrealdb-core `syn/parser/stmt/define.rs`,
		// `parse_define_index`); `SEARCH ANALYZER` and `MTREE` are the pre-3.0
		// spellings, kept so 2.x schemas still parse.
		IndexClause: ($) =>
			choice(
				$.UniqueClause,
				$.CountClause,
				$.FullTextClause,
				$.SearchAnalyzerClause,
				$.MtreeClause,
				$.HnswClause,
				$.DiskAnnClause,
			),
		UniqueClause: ($) => alias($._kw_unique, $.Keyword),

		// `COUNT [WHERE <condition>]`. The condition is optional: a bare
		// `COUNT` is an unconditional count index.
		CountClause: ($) =>
			seq(alias($._kw_count, $.Keyword), optional($.WhereClause)),

		// `FULLTEXT [ANALYZER <name>] [BM25 [(<k1>, <b>)]] [HIGHLIGHTS]`. The
		// engine accepts the three options in any order and requires none of
		// them — an absent analyzer falls back to `like`.
		FullTextClause: ($) =>
			seq(
				alias($._kw_fulltext, $.Keyword),
				repeat(
					choice(
						seq(alias($._kw_analyzer, $.Keyword), $.Ident),
						$.Bm25Clause,
						alias($._kw_highlights, $.Keyword),
					),
				),
			),

		SearchAnalyzerClause: ($) =>
			seq(
				alias($._kw_search, $.Keyword),
				alias($._kw_analyzer, $.Keyword),
				$.Ident,
				repeat(
					choice(
						$.Bm25Clause,
						$.DocIdsOrderClause,
						$.DocLenghtsOrderClause,
						$.PostingsOrderClause,
						$.TermsOrderClause,
						$.DocIdsCacheClause,
						$.DocLenghtsCacheClause,
						$.PostingsCacheClause,
						$.TermsCacheClause,
						alias($._kw_highlights, $.Keyword),
					),
				),
			),

		Bm25Clause: ($) =>
			seq(
				alias($._kw_bm25, $.Keyword),
				optional(seq('(', $.Number, ',', $.Number, ')')),
			),
		DocIdsCacheClause: ($) =>
			seq(alias($._kw_doc_ids_cache, $.Keyword), $.Number),
		DocIdsOrderClause: ($) =>
			seq(alias($._kw_doc_ids_order, $.Keyword), $.Number),
		DocLenghtsCacheClause: ($) =>
			seq(alias($._kw_doc_lengths_cache, $.Keyword), $.Number),
		DocLenghtsOrderClause: ($) =>
			seq(alias($._kw_doc_lengths_order, $.Keyword), $.Number),
		PostingsCacheClause: ($) =>
			seq(alias($._kw_postings_cache, $.Keyword), $.Number),
		PostingsOrderClause: ($) =>
			seq(alias($._kw_postings_order, $.Keyword), $.Number),
		TermsCacheClause: ($) =>
			seq(alias($._kw_terms_cache, $.Keyword), $.Number),
		TermsOrderClause: ($) =>
			seq(alias($._kw_terms_order, $.Keyword), $.Number),

		MtreeClause: ($) =>
			seq(
				alias($._kw_mtree, $.Keyword),
				$.IndexDimensionClause,
				repeat(
					choice(
						$.MtreeDistClause,
						$.IndexTypeClause,
						$.IndexCapacityClause,
						$.DocIdsOrderClause,
						$.DocIdsCacheClause,
						$.MtreeCacheClause,
					),
				),
			),
		MtreeCacheClause: ($) =>
			seq(alias($._kw_mtree_cache, $.Keyword), $.Number),
		MtreeDistClause: ($) => seq(alias($._kw_dist, $.Keyword), $.Distance),

		HnswClause: ($) =>
			seq(
				alias($._kw_hnsw, $.Keyword),
				$.IndexDimensionClause,
				repeat(
					choice(
						$.HnswDistClause,
						$.IndexTypeClause,
						$.IndexCapacityClause,
						$.IndexLmClause,
						$.IndexM0Clause,
						$.IndexMClause,
						$.IndexEfcClause,
						$.IndexExtendCandidatesClause,
						$.IndexKeepPrunedConnectionsClause,
						$.IndexHashedVectorClause,
					),
				),
			),
		// The engine lexes `DIST` and `DISTANCE` as the same keyword.
		HnswDistClause: ($) =>
			seq(
				choice(
					alias($._kw_dist, $.Keyword),
					alias($._kw_distance, $.Keyword),
				),
				choice(
					$.Distance,
					seq(alias($._kw_minkowski, $.Distance), $.Number),
				),
			),

		// `DISKANN DIMENSION <n> [DIST <d>] [TYPE <t>] [DEGREE <n>]
		// [L_BUILD <n>] [ALPHA <n>] [HASHED_VECTOR]` (SurrealDB 3.x).
		DiskAnnClause: ($) =>
			seq(
				alias($._kw_diskann, $.Keyword),
				$.IndexDimensionClause,
				repeat(
					choice(
						$.DiskAnnDistClause,
						$.IndexTypeClause,
						$.IndexDegreeClause,
						$.IndexLBuildClause,
						$.IndexAlphaClause,
						$.IndexHashedVectorClause,
					),
				),
			),
		DiskAnnDistClause: ($) =>
			seq(
				choice(
					alias($._kw_dist, $.Keyword),
					alias($._kw_distance, $.Keyword),
				),
				choice(
					$.Distance,
					seq(alias($._kw_minkowski, $.Distance), $.Number),
				),
			),

		IndexDimensionClause: ($) =>
			seq(alias($._kw_dimension, $.Keyword), $.Number),
		IndexCapacityClause: ($) =>
			seq(alias($._kw_capacity, $.Keyword), $.Number),
		IndexLmClause: ($) => seq(alias($._kw_lm, $.Keyword), $.Number),
		IndexM0Clause: ($) => seq(alias($._kw_m0, $.Keyword), $.Number),
		IndexMClause: ($) => seq(alias($._kw_m, $.Keyword), $.Number),
		IndexEfcClause: ($) => seq(alias($._kw_efc, $.Keyword), $.Number),
		IndexExtendCandidatesClause: ($) =>
			alias($._kw_extend_candidates, $.Keyword),
		IndexKeepPrunedConnectionsClause: ($) =>
			alias($._kw_keep_pruned_connections, $.Keyword),
		IndexHashedVectorClause: ($) => alias($._kw_hashed_vector, $.Keyword),
		IndexDegreeClause: ($) => seq(alias($._kw_degree, $.Keyword), $.Number),
		IndexLBuildClause: ($) => seq(alias($._kw_l_build, $.Keyword), $.Number),
		IndexAlphaClause: ($) => seq(alias($._kw_alpha, $.Keyword), $.Number),

		// Define table
		TableTypeClause: ($) =>
			seq(
				alias($._kw_type, $.Keyword),
				choice(
					alias($._kw_any, $.Keyword),
					alias($._kw_normal, $.Keyword),
					seq(
						alias($._kw_relation, $.Keyword),
						optional(
							seq(
								choice(
									alias($._kw_in, $.Keyword),
									alias($._kw_from, $.Keyword),
								),
								piped($.Ident),
							),
						),
						optional(
							seq(
								choice(
									alias($._kw_out, $.Keyword),
									alias($._kw_to, $.Keyword),
								),
								piped($.Ident),
							),
						),
						optional($.EnforcedClause),
					),
				),
			),
		EnforcedClause: ($) => alias($._kw_enforced, $.Keyword),

		TableViewClause: ($) =>
			seq(
				alias($._kw_as, $.Keyword),
				alias($._kw_select, $.Keyword),
				csep($._inclusivePredicate),
				alias($._kw_from, $.Keyword),
				csep($._value),
				optional($.WhereClause),
				optional($.GroupClause),
			),

		ChangefeedClause: ($) =>
			seq(alias($._kw_changefeed, $.Keyword), $.Duration),

		WhenClause: ($) => seq(alias($._kw_when, $.Keyword), $._value),
		ThenClause: ($) =>
			seq(
				optional(alias($._kw_async, $.Keyword)),
				alias($._kw_then, $.Keyword),
				csep(choice($.SubQuery, $.Block)),
			),

		TokenizersClause: ($) =>
			seq(alias($._kw_tokenizers, $.Keyword), csep($.AnalyzerTokenizer)),
		FiltersClause: ($) =>
			seq(alias($._kw_filters, $.Keyword), csep($.AnalyzerFilters)),
		FunctionClause: ($) =>
			seq(alias($._kw_function, $.Keyword), $.FunctionName),

		TypeClause: ($) =>
			prec.right(
				choice(
					seq(
						alias($._kw_flexible, $.Keyword),
						alias($._kw_type, $.Keyword),
						$._type,
					),
					seq(
						alias($._kw_type, $.Keyword),
						$._type,
						alias($._kw_flexible, $.Keyword),
					),
					seq(alias($._kw_type, $.Keyword), $._type),
				),
			),

		DefaultClause: ($) =>
			seq(
				alias($._kw_default, $.Keyword),
				optional($.DefaultAlways),
				$._value,
			),
		DefaultAlways: ($) => alias($._kw_always, $.Keyword),

		ReadonlyClause: ($) => alias($._kw_readonly, $.Keyword),
		ValueClause: ($) => seq(alias($._kw_value, $.Keyword), $._value),
		AssertClause: ($) => seq(alias($._kw_assert, $.Keyword), $._value),
		ComputedClause: ($) => seq(alias($._kw_computed, $.Keyword), $._value),

		ReferenceClause: ($) =>
			seq(
				alias($._kw_reference, $.Keyword),
				optional(
					seq(
						alias($._kw_on, $.Keyword),
						alias($._kw_delete, $.Keyword),
						choice(
							alias($._kw_reject, $.Keyword),
							alias($._kw_cascade, $.Keyword),
							alias($._kw_ignore, $.Keyword),
							alias($._kw_unset, $.Keyword),
							seq(alias($._kw_then, $.Keyword), $.Block),
						),
					),
				),
			),

		PermissionGroup: ($) =>
			seq(
				alias($._kw_for, $.Keyword),
				csep(
					choice(
						alias($._kw_select, $.Keyword),
						alias($._kw_create, $.Keyword),
						alias($._kw_update, $.Keyword),
						alias($._kw_delete, $.Keyword),
					),
				),
				choice(
					$.WhereClause,
					alias($._kw_none, $.None),
					alias($._kw_full, $.Literal),
				),
			),

		PermissionsForClause: ($) =>
			seq(
				alias($._kw_permissions, $.Keyword),
				choice(
					alias($._kw_none, $.None),
					alias($._kw_full, $.Literal),
					repeat1($.PermissionGroup),
				),
			),

		PermissionsBasicClause: ($) =>
			seq(
				alias($._kw_permissions, $.Keyword),
				choice(
					alias($._kw_none, $.None),
					alias($._kw_full, $.Literal),
					$.WhereClause,
				),
			),

		MiddlewareClause: ($) =>
			seq(alias($._kw_middleware, $.Keyword), csep($.FunctionCall)),
		CommentClause: ($) => seq(alias($._kw_comment, $.Keyword), $.String),
		BackendClause: ($) => seq(alias($._kw_backend, $.Keyword), $._value),

		AnalyzerFilters: ($) =>
			seq(
				alias($._analyzerFilterKw, $.Filter),
				optional(
					seq(
						'(',
						choice(seq($.Number, ',', $.Number), $.Ident),
						')',
					),
				),
			),

		// ================================================================
		// Values
		// ================================================================

		_value: ($) =>
			choice(
				$.Path,
				$.BinaryExpression,
				$.Range,
				$.PrefixExpression,
				$._baseValue,
			),

		// `!`, and the arithmetic signs. A sign in front of a literal number
		// stays part of the `Number` token (see `Number` below); everywhere
		// else — `-$x`, `-[1, 2, 3]`, `-fn::f()` — it is a prefix operator,
		// which is how surrealdb-core reads it.
		PrefixExpression: ($) =>
			prec(
				'prefix',
				seq(
					choice(
						alias('!', $.Operator),
						alias('-', $.Operator),
						alias('+', $.Operator),
					),
					$._prefixOperand,
				),
			),
		_prefixOperand: ($) => choice($.PrefixExpression, $.Path, $._baseValue),

		_baseValue: ($) =>
			choice(
				$._computedValue,
				$.FormatString,
				$.Regex,
				$.VariableName,
				$.FunctionJs,
				$.FunctionCall,
				$.SubQuery,
				$.Block,
				$.Closure,
				$.TypeCast,
				$.Ident,
			),

		_computedValue: ($) =>
			choice(
				$.String,
				$.Number,
				alias($._kw_true, $.Bool),
				alias($._kw_false, $.Bool),
				alias($._kw_null, $.None),
				alias($._kw_none, $.None),
				$.Array,
				$.Set,
				$.RecordId,
				$.Object,
				$.Duration,
				$.Point,
				// `|table:10|` and `|table:1..10|` generate records anywhere a
				// value is wanted, not only as a CREATE target.
				$.RangeRecordId,
			),

		// Paths
		Path: ($) =>
			choice(
				seq($._baseValue, repeat1($._pathElement)),
				seq(
					$.At,
					choice(
						seq($._dotPart, repeat($._pathElement)),
						repeat1($._pathElement),
					),
				),
				seq($.Lookup, repeat($._pathElement)),
			),
		_pathElement: ($) =>
			choice($.Lookup, $.Subscript, alias($._pathFilter, $.Filter)),
		Subscript: ($) => seq('.', $._dotPart),
		_dotPart: ($) =>
			choice(
				$.At,
				$.Ident,
				$.IdiomFunction,
				alias('*', $.Any),
				$.Optional,
				$.Destructure,
				$.Recurse,
			),

		_pathFilter: ($) =>
			seq(
				'[',
				choice(
					$.WhereClause,
					// `[? value]` shorthand — wrap in WhereClause to match lezer's
					// inline `WhereClause { "?" value }` rule.
					alias($._questionWhere, $.WhereClause),
					// `[*]` selects every element, `[$]` the last one.
					alias('*', $.Any),
					alias('$', $.Last),
					$._expression,
				),
				']',
			),
		_questionWhere: ($) => seq('?', $._value),

		Lookup: ($) =>
			seq(
				choice($.LookupRight, $.LookupLeft, $.LookupBoth),
				choice($.Ident, $.Any, $.LookupSelection),
			),

		LookupSelection: ($) =>
			seq(
				'(',
				optional($.GraphFieldSelection),
				csep($.GraphPredicate),
				repeat(
					choice(
						$.WhereClause,
						alias($.SplitClause, $.GraphSplitClause),
						alias($.GroupClause, $.GraphGroupClause),
						alias($.OrderClause, $.GraphOrderClause),
						alias(
							$.LimitStartComboClause,
							$.GraphLimitStartComboClause,
						),
						seq(alias($._kw_as, $.Keyword), $.Ident),
					),
				),
				')',
			),
		GraphFieldSelection: ($) =>
			seq(
				alias($._kw_select, $.Keyword),
				$.Fields,
				alias($._kw_from, $.Keyword),
			),
		GraphPredicate: ($) => choice($._value, $.Any),

		Destructure: ($) =>
			seq(
				$.BraceOpen,
				csep(
					choice(
						seq(
							$.Ident,
							$.Colon,
							choice(
								seq($.Lookup, repeat($._pathElement)),
								$._value,
							),
						),
						seq(choice($.Ident, $.Lookup), repeat($._pathElement)),
					),
				),
				$.BraceClose,
			),

		IdiomFunction: ($) =>
			seq(alias($._rawident, $.FunctionName), $.ArgumentList),

		Recurse: ($) =>
			seq(
				$.BraceOpen,
				$.RecurseRange,
				optional($.RecurseOptions),
				$.BraceClose,
				optional(seq('(', repeat1($._pathElement), ')')),
			),
		RecurseRange: ($) =>
			prec.right(
				choice(
					seq($.Int, $.RangeOp, $.Int),
					seq($.Int, $.RangeOp),
					$.RangeOp,
					seq($.RangeOp, $.Int),
					$.Int,
				),
			),
		RecurseOptions: ($) =>
			repeat1(
				seq(
					'+',
					alias($._rawident, $.FunctionName),
					optional(seq('=', $._baseValue)),
				),
			),

		// Idiom
		Idiom: ($) =>
			seq($.Ident, repeat(seq('.', choice($.Ident, alias('*', $.Any))))),

		// Binary expression
		//
		// Operators are grouped into precedence tiers whose order mirrors
		// surrealdb-core's `BindingPower` enum, tightest to loosest:
		// power > multiplicative > additive > relation > equality >
		// conjunction (AND) > disjunction (OR) > nullish (?? / ?:).
		// Each tier still surfaces a single `Operator` node, so the CST node
		// types are unchanged — only the nesting is corrected. This is what
		// makes `a > 1 AND b > 2` parse as `(a > 1) AND (b > 2)` rather than
		// the previous flat-left `((a > 1) AND b) > 2`, and it also gives the
		// engine-faithful `a = (b < c)` and `a ?: (b OR c)` nestings.
		BinaryExpression: ($) => {
			const tier = (level, ops) =>
				prec.left(
					level,
					seq($._value, alias(ops, $.Operator), $._value),
				);
			return choice(
				tier('binary_nullish', $._binop_nullish),
				tier('binary_disjunction', $._binop_disjunction),
				tier('binary_conjunction', $._binop_conjunction),
				tier('binary_equality', $._binop_equality),
				tier('binary_relation', $._binop_relation),
				tier('binary_additive', $._binop_additive),
				tier('binary_multiplicative', $._binop_multiplicative),
				tier('binary_power', $._binop_power),
			);
		},

		// Nullish coalescing / ternary — looser than OR (BindingPower::Nullish).
		_binop_nullish: ($) => choice('??', '?:'),
		_binop_disjunction: ($) => choice($._kw_or, '||'),
		_binop_conjunction: ($) => choice($._kw_and, '&&'),
		// Equality family (BindingPower::Equality): =, ==, !=, ?=, *=, IS,
		// IS NOT, the fuzzy-match operators, and the full-text @@ / @ref@.
		_binop_equality: ($) =>
			choice(
				'=',
				'==',
				'!=',
				'?=',
				'*=',
				'~',
				'!~',
				'*~',
				$._kw_is,
				seq($._kw_is, $._kw_not),
				'@@',
				seq('@', $.Number, '@'),
			),
		// Relational family (BindingPower::Relation): ordering, membership,
		// containment, geo, and the KNN operator.
		_binop_relation: ($) =>
			choice(
				'<',
				'<=',
				'>',
				'>=',
				alias($._kw_in, $.Keyword),
				seq($._kw_not, alias($._kw_in, $.Keyword)),
				$._kw_contains,
				$._kw_containsnot,
				$._kw_containsall,
				$._kw_containsany,
				$._kw_containsnone,
				$._kw_inside,
				$._kw_notinside,
				$._kw_allinside,
				$._kw_anyinside,
				$._kw_noneinside,
				$._kw_outside,
				$._kw_intersects,
				seq(
					'<|',
					$.Number,
					optional(
						seq(
							',',
							choice(
								$.Number,
								$.Distance,
								seq(
									alias($._kw_minkowski, $.Distance),
									$.Number,
								),
							),
						),
					),
					'|>',
				),
				...['∋', '∌', '⊇', '⊃', '⊅', '∈', '∉', '⊆', '⊂', '⊄'],
			),
		_binop_additive: ($) => choice('+', '-', '+=', '-='),
		_binop_multiplicative: ($) => choice('*', '×', '/', '÷', '%'),
		_binop_power: ($) => '**',

		// Range
		Range: ($) =>
			prec.left(
				'range',
				choice(
					$.RangeOp,
					seq($._value, $.RangeOp),
					seq($.RangeOp, $._value),
					seq($._value, $.RangeOp, $._value),
				),
			),

		// Type cast
		TypeCast: ($) => seq('<', $._type, '>', $._baseValue),

		// Closure
		Closure: ($) =>
			prec(
				'closure',
				choice(
					seq(
						$.Pipe,
						optional(csep($.ParamDefinition)),
						$.Pipe,
						optional(seq($.LookupRight, $._type)),
						$.Block,
					),
					// Bare-expression body, e.g. `|$v, $i| $v` or `|$v| $v * 2`.
					// Any value is allowed, not just a BinaryExpression.
					seq(
						$.Pipe,
						optional(csep($.ParamDefinition)),
						$.Pipe,
						$._value,
					),
				),
			),

		// Inside a closure's `|…|` parameter list a bare `|` is the closing
		// pipe, so the type slot there is `_safeType`: a union has to be
		// bracketed, `|$x: <int | float>| $x`. The engine agrees — it answers
		// `Unexpected token `|`, expected Eof` for `|$x: int | float| $x`.
		ParamDefinition: ($) =>
			seq(
				$.VariableName,
				optional(seq($.Colon, alias($._safeType, $.Type))),
			),

		// The same node, in the two places the parameter list is not
		// pipe-delimited — `LET` and a `DEFINE FUNCTION` argument list — where
		// a bare `|` can only be a union and the engine accepts one:
		// `LET $a: int | float = 2` and
		// `DEFINE FUNCTION fn::g($x: int | float) { … }` both run.
		_unionParamDefinition: ($) =>
			seq($.VariableName, optional(seq($.Colon, alias($._type, $.Type)))),

		// Block / SubQuery
		Block: ($) => seq($.BraceOpen, optional($._expressions), $.BraceClose),

		SubQuery: ($) => seq('(', $._expression, ')'),

		// ----------------------------------------------------------------
		// Object/Array/Set/Point
		// ----------------------------------------------------------------

		Object: ($) =>
			seq(
				alias($._object_open, $.BraceOpen),
				optional($.ObjectContent),
				$.BraceClose,
			),
		ObjectContent: ($) => csepTrail($.ObjectProperty),
		ObjectProperty: ($) =>
			seq(
				$.ObjectKey,
				$.Colon,
				choice(
					alias($._objectSelectValue, $.SelectStatement),
					$._value,
				),
			),
		ObjectKey: ($) => choice(alias($._rawident, $.KeyName), $.String),

		_objectSelectValue: ($) =>
			seq(
				alias($._kw_select, $.Keyword),
				alias($._kw_value, $.Keyword),
				$.Predicate,
				alias($._kw_from, $.Keyword),
				optional(alias($._kw_only, $.Keyword)),
				$._value,
			),

		Array: ($) => seq('[', optional(csepTrail($._value)), ']'),

		Set: ($) =>
			seq(
				$.BraceOpen,
				choice(
					',',
					seq($._value, ','),
					seq($._value, ',', $._value, repeat(seq(',', $._value))),
				),
				$.BraceClose,
			),

		Point: ($) => seq('(', $.Number, ',', $.Number, ')'),

		// ----------------------------------------------------------------
		// Record ID
		// ----------------------------------------------------------------

		RecordId: ($) =>
			seq(
				alias($._idName, $.RecordTbIdent),
				$.Colon,
				choice($._recordIdValue, $.RecordIdRange),
			),
		RangeRecordId: ($) => seq($.Pipe, $.RecordId, $.Pipe),
		_idName: ($) => choice($._rawident, $._tickIdent, $._bracketIdent),
		RecordIdIdent: ($) =>
			choice($._numberident, $._tickIdent, $._bracketIdent),
		_recordIdValue: ($) =>
			choice($.RecordIdIdent, $.Array, $.Object, $.RecordIdString),
		// Lezer emits RecordIdString(String); we wrap the prefixed-string token
		// in an aliased String node to match the same structure.
		RecordIdString: ($) => alias($._prefixedString, $.String),
		RecordIdRange: ($) =>
			prec.right(
				choice(
					$.RangeOp,
					seq($._recordIdValue, $.RangeOp, $._recordIdValue),
					seq($._recordIdValue, $.RangeOp),
					seq($.RangeOp, $._recordIdValue),
				),
			),

		// ----------------------------------------------------------------
		// Function call (regular, custom, idiom-relative)
		// ----------------------------------------------------------------

		FunctionCall: ($) =>
			choice(
				seq(
					choice(
						$.FunctionName,
						alias($._kw_rand, $.FunctionName),
						alias($._kw_count, $.FunctionName),
					),
					optional($.Version),
					$.ArgumentList,
				),
				seq($.RecordId, $.ArgumentList),
				seq($.VariableName, $.ArgumentList),
			),
		ArgumentList: ($) =>
			seq(
				'(',
				optional(choice(csep($._value), $._subqueryStatement)),
				')',
			),
		Version: ($) => seq('<', $.VersionNumber, '>'),

		FunctionName: ($) =>
			choice(
				token(
					prec(
						3,
						seq(
							/[a-zA-Z_][a-zA-Z_0-9]*/,
							'::',
							/[a-zA-Z_][a-zA-Z_0-9]*/,
							repeat(seq('::', /[a-zA-Z_][a-zA-Z_0-9]*/)),
						),
					),
				),
				token(
					prec(
						3,
						seq('fn', repeat(seq('::', /[a-zA-Z_][a-zA-Z_0-9]*/))),
					),
				),
			),

		// ----------------------------------------------------------------
		// JS function
		// ----------------------------------------------------------------

		FunctionJs: ($) =>
			seq(
				alias($._kw_function, $.FunctionName),
				$.ArgumentList,
				$.JavaScriptBlock,
			),
		// The external scanner consumes the entire `{...}` block as one
		// token. We can't currently expose `BraceOpen`/`JavaScriptContent`/
		// `BraceClose` separately because the parser would invoke the scanner
		// in stray `{`-adjacent recovery states (e.g. after `[1f, 2f, …]`)
		// and silently eat the rest of the input. See the lezer-issues
		// catalog for the known divergence.
		JavaScriptBlock: ($) => $._js_function_body,

		// ----------------------------------------------------------------
		// Field assignment
		// ----------------------------------------------------------------

		// The target is an `Idiom`, not a bare `Ident`: the engine assigns to a
		// nested field, `CREATE person SET name.first = 'John'`, and
		// `DEFINE FIELD name.first ON person` already used `Idiom` here — so
		// without this a schema could declare a field no `SET` could assign.
		FieldAssignment: ($) =>
			seq(
				$.Idiom,
				alias($._assignmentOp, $.Operator),
				choice($.IfElseStatement, $._value),
			),
		_assignmentOp: ($) => choice('=', '+=', '-='),

		// ----------------------------------------------------------------
		// Fields & predicates
		// ----------------------------------------------------------------

		Fields: ($) =>
			choice(
				seq(alias($._kw_value, $.Keyword), $.Predicate),
				csep($._inclusivePredicate),
			),
		Predicate: ($) =>
			choice(
				$._value,
				seq($._value, alias($._kw_as, $.Keyword), $.Ident),
			),
		_inclusivePredicate: ($) => choice(alias('*', $.Any), $.Predicate),

		// ----------------------------------------------------------------
		// Types
		// ----------------------------------------------------------------

		_singleType: ($) =>
			choice(
				alias($._rawident, $.TypeName),
				$.ParameterizedType,
				$.LiteralType,
			),
		// `array<int, 3>` and `set<int, 5>` carry a length bound after the
		// element type; nothing else takes a second argument.
		ParameterizedType: ($) =>
			seq($._singleType, '<', $._type, optional(seq(',', $.Number)), '>'),
		_type: ($) => choice($._singleType, $.UnionType),
		UnionType: ($) =>
			prec.right(
				'union',
				seq($._singleType, repeat1(seq($.Pipe, $._singleType))),
			),
		_safeType: ($) => choice($._singleType, seq('<', $._type, '>')),

		LiteralType: ($) =>
			choice($.String, $.Number, $.Duration, $.ArrayType, $.ObjectType),
		ArrayType: ($) => seq('[', csep($._type), ']'),
		ObjectType: ($) =>
			seq(
				alias($._object_open, $.BraceOpen),
				optional($.ObjectTypeContent),
				$.BraceClose,
			),
		ObjectTypeContent: ($) => csep($.ObjectTypeProperty),
		ObjectTypeProperty: ($) => seq($.ObjectKey, $.Colon, $._type),

		// ================================================================
		// Lexical primitives
		// ================================================================

		Comment: ($) =>
			token(
				choice(
					seq('#', /[^\n]*/),
					seq('--', /[^\n]*/),
					seq('//', /[^\n]*/),
				),
			),

		BlockComment: ($) => token(seq('/*', /[^*]*\*+([^/*][^*]*\*+)*/, '/')),

		// A signed literal stays one `Number`, unchanged from before: the
		// dynamic precedence keeps `-1` a `Number(Int)` rather than a
		// `PrefixExpression` wrapping one, so no existing tree is reshaped.
		Number: ($) =>
			choice(
				prec.dynamic(1, seq(choice('-', '+'), $._unsignedNumber)),
				$._unsignedNumber,
			),
		_unsignedNumber: ($) => choice($.Decimal, $.Float, $.Int),

		Int: ($) => token(DIGITS),

		Float: ($) =>
			token(
				prec(
					1,
					choice(
						seq(DIGITS, 'f'),
						seq(
							DIGITS,
							choice(
								seq(
									'.',
									DIGITS,
									optional(/[eE][+-]?[0-9]+(?:_[0-9]+)*/),
								),
								/[eE][+-]?[0-9]+(?:_[0-9]+)*/,
							),
							optional('f'),
						),
						'Infinity',
						'NaN',
					),
				),
			),

		// Above `Float`'s precedence, because lexical precedence outranks
		// longest match: without it `102023.1dec` lexes as the float
		// `102023.1` followed by a stray `dec`.
		Decimal: ($) =>
			token(
				prec(
					2,
					seq(
						DIGITS,
						optional(seq('.', DIGITS)),
						optional(/[eE][+-]?[0-9]+(?:_[0-9]+)*/),
						'dec',
					),
				),
			),

		String: ($) => choice($._stringLiteral, $._prefixedString),
		// Lezer allows `\<newline>` and any other escape; we use [\s\S] to
		// include newlines after a backslash.
		_stringLiteral: ($) =>
			token(
				choice(
					seq("'", repeat(choice(/[^'\\]/, /\\[\s\S]/)), "'"),
					seq('"', repeat(choice(/[^"\\]/, /\\[\s\S]/)), '"'),
				),
			),
		_prefixedString: ($) =>
			token(
				prec(
					1,
					seq(
						/[rudbf]/,
						choice(
							seq("'", repeat(choice(/[^'\\]/, /\\[\s\S]/)), "'"),
							seq('"', repeat(choice(/[^"\\]/, /\\[\s\S]/)), '"'),
						),
					),
				),
			),

		Regex: ($) =>
			token(
				prec(
					-1,
					seq(
						'/',
						repeat1(
							choice(
								/[^/\\\n\[]/,
								seq('\\', /[^\n]/),
								seq(
									'[',
									repeat(
										choice(/[^\n\\\]]/, seq('\\', /[^\n]/)),
									),
									']',
								),
							),
						),
						optional(seq('/', /[dgimsuvy]*/)),
					),
				),
			),

		VariableName: ($) =>
			token(
				seq(
					'$',
					choice(
						/[a-zA-Z_][a-zA-Z0-9_]*/,
						seq('`', /[^`]+/, '`'),
						seq('⟨', /[^⟩]+/, '⟩'),
					),
				),
			),

		Duration: ($) => repeat1($.DurationPart),

		DurationPart: ($) =>
			token(
				seq(
					DIGITS,
					/\s*/,
					choice(
						'ns',
						'us',
						'µs',
						'ms',
						's',
						'm',
						'h',
						'd',
						'w',
						'y',
					),
				),
			),

		// Format string with structured Interpolation nodes (mirrors lezer's
		// `FormatString { '$"' (content | Interpolation)* '"' | ... }`). The
		// content tokens use `prec(-1)` so they never outrank a real
		// expression-level token that could appear after error recovery.
		FormatString: ($) =>
			choice(
				seq(
					'$"',
					repeat(choice($._formatStringTextDouble, $.Interpolation)),
					'"',
				),
				seq(
					"$'",
					repeat(choice($._formatStringTextSingle, $.Interpolation)),
					"'",
				),
			),
		_formatStringTextDouble: ($) => token(prec(-1, /([^"\\{]|\\[\s\S])+/)),
		_formatStringTextSingle: ($) => token(prec(-1, /([^'\\{]|\\[\s\S])+/)),
		Interpolation: ($) => seq($.BraceOpen, $._expression, $.BraceClose),

		Ident: ($) => $._idName,

		_rawident: ($) => token(prec(-1, /[a-zA-Z_][a-zA-Z0-9_]*/)),
		_tickIdent: ($) => token(seq('`', /[^`]+/, '`')),
		_bracketIdent: ($) => token(seq('⟨', /[^⟩]+/, '⟩')),
		_numberident: ($) =>
			token(choice(/[a-zA-Z_][a-zA-Z0-9_]*/, /[0-9][a-zA-Z0-9_]*/)),

		VersionNumber: ($) =>
			token(
				seq(
					DIGITS,
					optional(seq('.', DIGITS, optional(seq('.', DIGITS)))),
				),
			),

		// ================================================================
		// Visible token-as-node rules
		// ================================================================

		Keyword: ($) => $._any_kw,
		// Operator. Mirrors lezer's tree shape:
		//   - In lezer the `in` keyword has `[@name=Keyword]` (visible) — so
		//     `Operator(Keyword)` for `IN`. `is`, `not`, and the
		//     `binaryOperatorKeyword` group (AND, OR, CONTAINS, …) are
		//     internal extend tokens without an `@name`, so the keyword text
		//     is consumed but not shown in the tree: `Operator` only.
		// The binary-operator alphabet, kept as a single symbol so the `!`
		// prefix and field assignments can still alias to `$.Operator`. Binary
		// expressions consume these via the precedence tiers above rather than
		// this rule directly.
		Operator: ($) =>
			choice(
				$._binop_nullish,
				$._binop_disjunction,
				$._binop_conjunction,
				$._binop_equality,
				$._binop_relation,
				$._binop_additive,
				$._binop_multiplicative,
				$._binop_power,
			),
		RangeOp: ($) => choice('..', '..=', '>..', '>..='),
		BraceOpen: ($) => '{',
		BraceClose: ($) => '}',
		Colon: ($) => ':',
		Pipe: ($) => '|',
		LookupRight: ($) => '->',
		LookupLeft: ($) => choice('<-', '<~'),
		LookupBoth: ($) => '<->',
		Any: ($) => choice('?', '*'),
		At: ($) => '@',
		Optional: ($) => '?',
		Bool: ($) => choice($._kw_true, $._kw_false),
		None: ($) => choice($._kw_null, $._kw_none),
		Literal: ($) =>
			choice($._kw_after, $._kw_before, $._kw_diff, $._kw_full),

		Distance: ($) =>
			choice(
				$._kw_chebyshev,
				$._kw_cosine,
				$._kw_cosine_normalized,
				$._kw_euclidean,
				$._kw_hamming,
				$._kw_inner_product,
				$._kw_jaccard,
				$._kw_manhattan,
				$._kw_minkowski,
				$._kw_pearson,
			),

		_analyzerFilterKw: ($) =>
			choice(
				$._kw_ascii,
				$._kw_edgengram,
				$._kw_ngram,
				$._kw_snowball,
				$._kw_uppercase,
				$._kw_lowercase,
			),

		AnalyzerTokenizer: ($) =>
			choice($._kw_blank, $._kw_camel, $._kw_class, $._kw_punct),

		TokenType: ($) =>
			choice(
				$._kw_jwks,
				$._kw_eddsa,
				$._kw_es256,
				$._kw_es384,
				$._kw_es512,
				$._kw_hs256,
				$._kw_hs384,
				$._kw_hs512,
				$._kw_ps256,
				$._kw_ps384,
				$._kw_ps512,
				$._kw_rs256,
				$._kw_rs384,
				$._kw_rs512,
			),

		HttpMethod: ($) =>
			choice(
				$._kw_get,
				$._kw_put,
				$._kw_post,
				$._kw_delete,
				$._kw_patch,
				$._kw_trace,
			),

		IndexTypeClause: ($) =>
			seq(
				optional(alias($._kw_type, $.Keyword)),
				choice(
					alias($._kw_f16, $.Keyword),
					alias($._kw_f32, $.Keyword),
					alias($._kw_f64, $.Keyword),
					alias($._kw_i8, $.Keyword),
					alias($._kw_i16, $.Keyword),
					alias($._kw_i32, $.Keyword),
					alias($._kw_i64, $.Keyword),
					alias($._kw_u8, $.Keyword),
				),
			),

		// ================================================================
		// Hidden token-source rules
		// ================================================================

		// ================================================================
		// Keyword tokens
		// ================================================================

		_kw_true: ($) => kw('true'),
		_kw_false: ($) => kw('false'),
		_kw_null: ($) => kw('null'),
		_kw_none: ($) => kw('none'),
		_kw_after: ($) => kw('after'),
		_kw_before: ($) => kw('before'),
		_kw_diff: ($) => kw('diff'),
		_kw_full: ($) => kw('full'),

		_kw_access: ($) => kw('access'),
		_kw_algorithm: ($) => kw('algorithm'),
		_kw_all: ($) => kw('all'),
		_kw_alter: ($) => kw('alter'),
		_kw_always: ($) => kw('always'),
		_kw_analyzer: ($) => kw('analyzer'),
		_kw_and: ($) => kw('and'),
		_kw_any: ($) => kw('any'),
		_kw_api: ($) => kw('api'),
		_kw_as: ($) => kw('as'),
		_kw_asc: ($) => kw('asc'),
		_kw_assert: ($) => kw('assert'),
		_kw_at: ($) => kw('at'),
		_kw_async: ($) => kw('async'),
		_kw_authenticate: ($) => kw('authenticate'),
		_kw_auto: ($) => kw('auto'),
		_kw_backend: ($) => kw('backend'),
		_kw_begin: ($) => kw('begin'),
		_kw_bm25: ($) => kw('bm25'),
		_kw_break: ($) => kw('break'),
		_kw_bucket: ($) => kw('bucket'),
		_kw_by: ($) => kw('by'),
		_kw_cancel: ($) => kw('cancel'),
		_kw_capacity: ($) => kw('capacity'),
		_kw_cascade: ($) => kw('cascade'),
		_kw_changefeed: ($) => kw('changefeed'),
		_kw_changes: ($) => kw('changes'),
		_kw_collate: ($) => kw('collate'),
		_kw_columns: ($) => kw('columns'),
		_kw_comment: ($) => kw('comment'),
		_kw_commit: ($) => kw('commit'),
		_kw_computed: ($) => kw('computed'),
		_kw_concurrently: ($) => kw('concurrently'),
		_kw_config: ($) => kw('config'),
		_kw_content: ($) => kw('content'),
		_kw_continue: ($) => kw('continue'),
		_kw_create: ($) => kw('create'),
		_kw_database: ($) => kw('database'),
		_kw_db: ($) => kw('db'),
		_kw_default: ($) => kw('default'),
		_kw_defer: ($) => kw('defer'),
		_kw_define: ($) => kw('define'),
		_kw_delete: ($) => kw('delete'),
		_kw_desc: ($) => kw('desc'),
		_kw_dimension: ($) => kw('dimension'),
		_kw_dist: ($) => kw('dist'),
		_kw_distance: ($) => kw('distance'),
		_kw_doc_ids_cache: ($) => kw('doc_ids_cache'),
		_kw_doc_ids_order: ($) => kw('doc_ids_order'),
		_kw_doc_lengths_cache: ($) => kw('doc_lengths_cache'),
		_kw_doc_lengths_order: ($) => kw('doc_lengths_order'),
		_kw_drop: ($) => kw('drop'),
		_kw_duplicate: ($) => kw('duplicate'),
		_kw_duration: ($) => kw('duration'),
		_kw_efc: ($) => kw('efc'),
		_kw_else: ($) => kw('else'),
		_kw_end: ($) => kw('end'),
		_kw_enforced: ($) => kw('enforced'),
		_kw_event: ($) => kw('event'),
		_kw_exclude: ($) => kw('exclude'),
		_kw_exists: ($) => kw('exists'),
		_kw_explain: ($) => kw('explain'),
		_kw_expunge: ($) => kw('expunge'),
		_kw_extend_candidates: ($) => kw('extend_candidates'),
		_kw_fetch: ($) => kw('fetch'),
		_kw_field: ($) => kw('field'),
		_kw_fields: ($) => kw('fields'),
		_kw_filters: ($) => kw('filters'),
		_kw_flexible: ($) => kw('flexible'),
		_kw_for: ($) => kw('for'),
		_kw_from: ($) => kw('from'),
		_kw_function: ($) => kw('function'),
		_kw_functions: ($) => kw('functions'),
		_kw_get: ($) => kw('get'),
		_kw_graphql: ($) => kw('graphql'),
		_kw_group: ($) => kw('group'),
		_kw_highlights: ($) => kw('highlights'),
		_kw_hnsw: ($) => kw('hnsw'),
		_kw_if: ($) => kw('if'),
		_kw_ignore: ($) => kw('ignore'),
		_kw_in: ($) => kw('in'),
		_kw_include: ($) => kw('include'),
		_kw_index: ($) => kw('index'),
		_kw_info: ($) => kw('info'),
		_kw_insert: ($) => kw('insert'),
		_kw_into: ($) => kw('into'),
		_kw_is: ($) => kw('is'),
		_kw_issuer: ($) => kw('issuer'),
		_kw_jwt: ($) => kw('jwt'),
		_kw_keep_pruned_connections: ($) => kw('keep_pruned_connections'),
		_kw_key: ($) => kw('key'),
		_kw_kill: ($) => kw('kill'),
		_kw_let: ($) => kw('let'),
		_kw_limit: ($) => kw('limit'),
		_kw_live: ($) => kw('live'),
		_kw_lm: ($) => kw('lm'),
		_kw_m: ($) => kw('m'),
		_kw_m0: ($) => kw('m0'),
		_kw_merge: ($) => kw('merge'),
		_kw_middleware: ($) => kw('middleware'),
		_kw_mtree: ($) => kw('mtree'),
		_kw_mtree_cache: ($) => kw('mtree_cache'),
		_kw_namespace: ($) => kw('namespace'),
		_kw_noindex: ($) => kw('noindex'),
		_kw_normal: ($) => kw('normal'),
		_kw_not: ($) => kw('not'),
		_kw_ns: ($) => kw('ns'),
		_kw_numeric: ($) => kw('numeric'),
		_kw_omit: ($) => kw('omit'),
		_kw_on: ($) => kw('on'),
		_kw_only: ($) => kw('only'),
		_kw_option: ($) => kw('option'),
		_kw_or: ($) => kw('or'),
		_kw_order: ($) => kw('order'),
		_kw_out: ($) => kw('out'),
		_kw_overwrite: ($) => kw('overwrite'),
		_kw_parallel: ($) => kw('parallel'),
		_kw_param: ($) => kw('param'),
		_kw_passhash: ($) => kw('passhash'),
		_kw_password: ($) => kw('password'),
		_kw_patch: ($) => kw('patch'),
		_kw_permissions: ($) => kw('permissions'),
		_kw_post: ($) => kw('post'),
		_kw_postings_cache: ($) => kw('postings_cache'),
		_kw_postings_order: ($) => kw('postings_order'),
		_kw_put: ($) => kw('put'),
		_kw_readonly: ($) => kw('readonly'),
		_kw_rebuild: ($) => kw('rebuild'),
		_kw_record: ($) => kw('record'),
		_kw_reference: ($) => kw('reference'),
		_kw_reject: ($) => kw('reject'),
		_kw_relate: ($) => kw('relate'),
		_kw_relation: ($) => kw('relation'),
		_kw_remove: ($) => kw('remove'),
		_kw_replace: ($) => kw('replace'),
		_kw_return: ($) => kw('return'),
		_kw_roles: ($) => kw('roles'),
		_kw_root: ($) => kw('root'),
		_kw_sc: ($) => kw('sc'),
		_kw_schemafull: ($) => kw('schemafull'),
		_kw_schemaless: ($) => kw('schemaless'),
		_kw_scope: ($) => kw('scope'),
		_kw_search: ($) => kw('search'),
		_kw_select: ($) => kw('select'),
		_kw_session: ($) => kw('session'),
		_kw_set: ($) => kw('set'),
		_kw_show: ($) => kw('show'),
		_kw_signin: ($) => kw('signin'),
		_kw_signup: ($) => kw('signup'),
		_kw_since: ($) => kw('since'),
		_kw_sleep: ($) => kw('sleep'),
		_kw_split: ($) => kw('split'),
		_kw_start: ($) => kw('start'),
		_kw_strict: ($) => kw('strict'),
		_kw_structure: ($) => kw('structure'),
		_kw_table: ($) => kw('table'),
		_kw_tables: ($) => kw('tables'),
		_kw_tb: ($) => kw('tb'),
		_kw_tempfiles: ($) => kw('tempfiles'),
		_kw_terms_cache: ($) => kw('terms_cache'),
		_kw_terms_order: ($) => kw('terms_order'),
		_kw_then: ($) => kw('then'),
		_kw_throw: ($) => kw('throw'),
		_kw_timeout: ($) => kw('timeout'),
		_kw_to: ($) => kw('to'),
		_kw_token: ($) => kw('token'),
		_kw_tokenizers: ($) => kw('tokenizers'),
		_kw_trace: ($) => kw('trace'),
		_kw_transaction: ($) => kw('transaction'),
		_kw_type: ($) => kw('type'),
		_kw_unique: ($) => kw('unique'),
		_kw_unset: ($) => kw('unset'),
		_kw_update: ($) => kw('update'),
		_kw_upsert: ($) => kw('upsert'),
		_kw_url: ($) => kw('url'),
		_kw_use: ($) => kw('use'),
		_kw_user: ($) => kw('user'),
		_kw_value: ($) => kw('value'),
		_kw_values: ($) => kw('values'),
		_kw_version: ($) => kw('version'),
		_kw_when: ($) => kw('when'),
		_kw_where: ($) => kw('where'),
		_kw_with: ($) => kw('with'),

		// Operator keywords
		_kw_contains: ($) => kw('contains'),
		_kw_containsnot: ($) => kw('containsnot'),
		_kw_containsall: ($) => kw('containsall'),
		_kw_containsany: ($) => kw('containsany'),
		_kw_containsnone: ($) => kw('containsnone'),
		_kw_inside: ($) => kw('inside'),
		_kw_notinside: ($) => kw('notinside'),
		_kw_allinside: ($) => kw('allinside'),
		_kw_anyinside: ($) => kw('anyinside'),
		_kw_noneinside: ($) => kw('noneinside'),
		_kw_outside: ($) => kw('outside'),
		_kw_intersects: ($) => kw('intersects'),

		// Distance keywords
		_kw_chebyshev: ($) => kw('chebyshev'),
		_kw_cosine: ($) => kw('cosine'),
		_kw_cosine_normalized: ($) => kw('cosine_normalized'),
		_kw_euclidean: ($) => kw('euclidean'),
		_kw_hamming: ($) => kw('hamming'),
		_kw_inner_product: ($) => kw('inner_product'),
		_kw_jaccard: ($) => kw('jaccard'),
		_kw_manhattan: ($) => kw('manhattan'),
		_kw_minkowski: ($) => kw('minkowski'),
		_kw_pearson: ($) => kw('pearson'),

		// Analyzer Filter keywords
		_kw_ascii: ($) => kw('ascii'),
		_kw_edgengram: ($) => kw('edgengram'),
		_kw_ngram: ($) => kw('ngram'),
		_kw_snowball: ($) => kw('snowball'),
		_kw_uppercase: ($) => kw('uppercase'),
		_kw_lowercase: ($) => kw('lowercase'),

		// Analyzer Tokenizer keywords
		_kw_blank: ($) => kw('blank'),
		_kw_camel: ($) => kw('camel'),
		_kw_class: ($) => kw('class'),
		_kw_punct: ($) => kw('punct'),

		// Token type keywords
		_kw_jwks: ($) => kw('jwks'),
		_kw_eddsa: ($) => kw('eddsa'),
		_kw_es256: ($) => kw('es256'),
		_kw_es384: ($) => kw('es384'),
		_kw_es512: ($) => kw('es512'),
		_kw_hs256: ($) => kw('hs256'),
		_kw_hs384: ($) => kw('hs384'),
		_kw_hs512: ($) => kw('hs512'),
		_kw_ps256: ($) => kw('ps256'),
		_kw_ps384: ($) => kw('ps384'),
		_kw_ps512: ($) => kw('ps512'),
		_kw_rs256: ($) => kw('rs256'),
		_kw_rs384: ($) => kw('rs384'),
		_kw_rs512: ($) => kw('rs512'),

		// Index vector type keywords (f16/f32/f64/i8/i16/i32/i64/u8)
		_kw_f16: ($) => kw('f16'),
		_kw_f32: ($) => kw('f32'),
		_kw_f64: ($) => kw('f64'),
		_kw_i8: ($) => kw('i8'),
		_kw_i16: ($) => kw('i16'),
		_kw_i32: ($) => kw('i32'),
		_kw_i64: ($) => kw('i64'),
		_kw_u8: ($) => kw('u8'),

		_kw_rand: ($) => kw('rand'),
		_kw_count: ($) => kw('count'),

		// Misc
		_kw_owner: ($) => kw('owner'),
		_kw_editor: ($) => kw('editor'),
		_kw_viewer: ($) => kw('viewer'),
		_kw_refresh: ($) => kw('refresh'),
		_kw_bearer: ($) => kw('bearer'),
		_kw_grant: ($) => kw('grant'),
		_kw_module: ($) => kw('module'),
		_kw_purge: ($) => kw('purge'),
		_kw_revoke: ($) => kw('revoke'),
		_kw_revoked: ($) => kw('revoked'),
		_kw_expired: ($) => kw('expired'),
		_kw_sequence: ($) => kw('sequence'),
		_kw_batch: ($) => kw('batch'),
		_kw_matches: ($) => kw('matches'),
		_kw_original: ($) => kw('original'),
		_kw_future: ($) => kw('future'),
		_kw_import: ($) => kw('import'),
		_kw_fulltext: ($) => kw('fulltext'),
		_kw_diskann: ($) => kw('diskann'),
		_kw_degree: ($) => kw('degree'),
		_kw_l_build: ($) => kw('l_build'),
		_kw_alpha: ($) => kw('alpha'),
		_kw_hashed_vector: ($) => kw('hashed_vector'),

		// Catch-all keyword union, used by visible Keyword rule
		_any_kw: ($) =>
			choice(
				$._kw_access,
				$._kw_algorithm,
				$._kw_all,
				$._kw_alter,
				$._kw_always,
				$._kw_analyzer,
				$._kw_and,
				$._kw_any,
				$._kw_api,
				$._kw_as,
				$._kw_asc,
				$._kw_assert,
				$._kw_at,
				$._kw_async,
				$._kw_authenticate,
				$._kw_auto,
				$._kw_backend,
				$._kw_begin,
				$._kw_bm25,
				$._kw_break,
				$._kw_bucket,
				$._kw_by,
				$._kw_cancel,
				$._kw_capacity,
				$._kw_cascade,
				$._kw_changefeed,
				$._kw_changes,
				$._kw_collate,
				$._kw_columns,
				$._kw_comment,
				$._kw_commit,
				$._kw_computed,
				$._kw_concurrently,
				$._kw_config,
				$._kw_content,
				$._kw_continue,
				$._kw_create,
				$._kw_database,
				$._kw_db,
				$._kw_default,
				$._kw_defer,
				$._kw_define,
				$._kw_delete,
				$._kw_desc,
				$._kw_dimension,
				$._kw_dist,
				$._kw_distance,
				$._kw_doc_ids_cache,
				$._kw_doc_ids_order,
				$._kw_doc_lengths_cache,
				$._kw_doc_lengths_order,
				$._kw_drop,
				$._kw_duplicate,
				$._kw_duration,
				$._kw_efc,
				$._kw_else,
				$._kw_end,
				$._kw_enforced,
				$._kw_event,
				$._kw_exclude,
				$._kw_exists,
				$._kw_explain,
				$._kw_expunge,
				$._kw_extend_candidates,
				$._kw_fetch,
				$._kw_field,
				$._kw_fields,
				$._kw_filters,
				$._kw_flexible,
				$._kw_for,
				$._kw_from,
				$._kw_function,
				$._kw_functions,
				$._kw_get,
				$._kw_graphql,
				$._kw_group,
				$._kw_highlights,
				$._kw_hnsw,
				$._kw_if,
				$._kw_ignore,
				$._kw_in,
				$._kw_include,
				$._kw_index,
				$._kw_info,
				$._kw_insert,
				$._kw_into,
				$._kw_is,
				$._kw_issuer,
				$._kw_jwt,
				$._kw_keep_pruned_connections,
				$._kw_key,
				$._kw_kill,
				$._kw_let,
				$._kw_limit,
				$._kw_live,
				$._kw_lm,
				$._kw_m,
				$._kw_m0,
				$._kw_merge,
				$._kw_middleware,
				$._kw_mtree,
				$._kw_mtree_cache,
				$._kw_namespace,
				$._kw_noindex,
				$._kw_normal,
				$._kw_not,
				$._kw_ns,
				$._kw_numeric,
				$._kw_omit,
				$._kw_on,
				$._kw_only,
				$._kw_option,
				$._kw_or,
				$._kw_order,
				$._kw_out,
				$._kw_overwrite,
				$._kw_parallel,
				$._kw_param,
				$._kw_passhash,
				$._kw_password,
				$._kw_patch,
				$._kw_permissions,
				$._kw_post,
				$._kw_postings_cache,
				$._kw_postings_order,
				$._kw_put,
				$._kw_readonly,
				$._kw_rebuild,
				$._kw_record,
				$._kw_reference,
				$._kw_reject,
				$._kw_relate,
				$._kw_relation,
				$._kw_remove,
				$._kw_replace,
				$._kw_return,
				$._kw_roles,
				$._kw_root,
				$._kw_sc,
				$._kw_schemafull,
				$._kw_schemaless,
				$._kw_scope,
				$._kw_search,
				$._kw_select,
				$._kw_session,
				$._kw_set,
				$._kw_show,
				$._kw_signin,
				$._kw_signup,
				$._kw_since,
				$._kw_sleep,
				$._kw_split,
				$._kw_start,
				$._kw_strict,
				$._kw_structure,
				$._kw_table,
				$._kw_tables,
				$._kw_tb,
				$._kw_tempfiles,
				$._kw_terms_cache,
				$._kw_terms_order,
				$._kw_then,
				$._kw_throw,
				$._kw_timeout,
				$._kw_to,
				$._kw_token,
				$._kw_tokenizers,
				$._kw_trace,
				$._kw_transaction,
				$._kw_type,
				$._kw_unique,
				$._kw_unset,
				$._kw_update,
				$._kw_upsert,
				$._kw_url,
				$._kw_use,
				$._kw_user,
				$._kw_value,
				$._kw_values,
				$._kw_version,
				$._kw_when,
				$._kw_where,
				$._kw_with,
			),
	},
});
