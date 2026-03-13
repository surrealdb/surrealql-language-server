
; Operators
[
  (binary_operator)
  (operator)
] @operator

; Literals
[
  (string)
  (prefixed_string)
] @string


[
  (int)
  (float)
  (decimal)
  (duration)
] @number

[
  (keyword_true)
  (keyword_false)
] @boolean


[
  (keyword_none)
  (keyword_null)
] @constant


; Comments
(comment) @comment


; Functions
(custom_function_name) @function

((function_name) @function
  (#match? @function "^[a-zA-Z_][a-zA-Z0-9_]*$"))

; Built-ins: highlight distinctly from user-defined functions, including
; in incomplete/error states where function_call may not be formed yet.
(builtin_function_name) @constant.builtin

((function_name) @constant.builtin
  (#match? @constant.builtin "^(array|crypto|duration|encoding|geo|http|math|meta|not|object|parse|rand|record|search|session|sleep|string|time|type|vector)::[a-zA-Z_][a-zA-Z0-9_]*(::[a-zA-Z_][a-zA-Z0-9_]*)?$"))

; Identifiers
((identifier) @keyword.control.conditional
  (#match? @keyword.control.conditional "^(?i:(if|end))$"))
((identifier) @variable
  (#not-match? @variable "^(?i:(if|end))$"))
((variable_name) @constant.builtin
  (#match? @constant.builtin "^\\$(this|auth|action|file|target|value|parent|access|event|before|after|request|reference|token|session|input)$"))
((variable_name) @variable.special
  (#not-match? @variable.special "^\\$(this|auth|action|file|target|value|parent|access|event|before|after|request|reference|token|session|input)$"))

; Properties
(object_property
    (object_key) @property)

(field_assignment
    (identifier) @property)


; Punctuation
[
  "("
  ")"
  "["
  "]"
  "<"
  ">"
  "{"
  "}"
] @punctuation.bracket
[
    ","
    ":"
] @punctuation.delimiter

[
    "="
] @operator


; Types
[
  (type)
  (type_name)
  (parameterized_type)
] @type


(record_id) @variable.special

; Special
(graph_path) @operator

; Keywords
[
  (keyword_select)
  (keyword_from)
  (keyword_else)
  (keyword_end)
  (keyword_only)
  (keyword_value)
  (keyword_as)
  (keyword_omit)
  (keyword_explain)
  (keyword_full)
  (keyword_parallel)
  (keyword_timeout)
  (keyword_fetch)
  (keyword_limit)
  (keyword_by)
  (keyword_rand)
  (keyword_collate)
  (keyword_numeric)
  (keyword_asc)
  (keyword_desc)
  (keyword_order)
  (keyword_with)
  (keyword_index)
  (keyword_no_index)
  (keyword_where)
  (keyword_split)
  (keyword_at)
  (keyword_group)
  (keyword_begin)
  (keyword_cancel)
  (keyword_commit)
  (keyword_transaction)
  (keyword_and)
  (keyword_or)
  (keyword_is)
  (keyword_not)
  (keyword_contains)
  (keyword_contains_not)
  (keyword_contains_all)
  (keyword_contains_any)
  (keyword_contains_none)
  (keyword_inside)
  (keyword_in)
  (keyword_not_inside)
  (keyword_all_inside)
  (keyword_any_inside)
  (keyword_none_inside)
  (keyword_outside)
  (keyword_intersects)
  (keyword_chebyshev)
  (keyword_cosine)
  (keyword_euclidean)
  (keyword_hamming)
  (keyword_jaccard)
  (keyword_manhattan)
  (keyword_minkowski)
  (keyword_pearson)
  (keyword_define)
  (keyword_analyzer)
  (keyword_event)
  (keyword_field)
  (keyword_function)
  (keyword_namespace)
  (keyword_param)
  (keyword_scope)
  (keyword_drop)
  (keyword_schemafull)
  (keyword_schemaless)
  (keyword_live)
  (keyword_diff)
  (keyword_flexible)
  (keyword_readonly)
  (keyword_jwks)
  (keyword_eddsa)
  (keyword_es256)
  (keyword_es384)
  (keyword_es512)
  (keyword_hs256)
  (keyword_hs384)
  (keyword_hs512)
  (keyword_ps256)
  (keyword_ps384)
  (keyword_ps512)
  (keyword_rs256)
  (keyword_rs384)
  (keyword_rs512)
  (keyword_bm25)
  (keyword_doc_ids_cache)
  (keyword_doc_ids_order)
  (keyword_doc_lengths_cache)
  (keyword_doc_lengths_order)
  (keyword_postings_cache)
  (keyword_postings_order)
  (keyword_terms_cache)
  (keyword_terms_order)
  (keyword_highlights)
  (keyword_any)
  (keyword_normal)
  (keyword_relation)
  (keyword_out)
  (keyword_to)
  (keyword_changefeed)
  (keyword_content)
  (keyword_merge)
  (keyword_patch)
  (keyword_before)
  (keyword_after)
  (keyword_table)
  (keyword_root)
  (keyword_token)
  (keyword_use)
  (keyword_ns)
  (keyword_db)
  (keyword_on)
  (keyword_user)
  (keyword_roles)
  (keyword_remove)
  (keyword_create)
  (keyword_delete)
  (keyword_update)
  (keyword_insert)
  (keyword_into)
  (keyword_tokenizers)
  (keyword_filters)
  (keyword_when)
  (keyword_then)
  (keyword_type)
  (keyword_default)
  (keyword_assert)
  (keyword_permissions)
  (keyword_relate)
  (keyword_ignore)
  (keyword_cascade)
  (keyword_reject)
  (keyword_values)
  (keyword_for)
  (keyword_info)
  (keyword_show)
  (keyword_changes)
  (keyword_since)
  (keyword_comment)
  (keyword_fields)
  (keyword_columns)
  (keyword_unique)
  (keyword_search)
  (keyword_session)
  (keyword_signin)
  (keyword_signup)
  (keyword_if)
  (keyword_exists)
  (keyword_database)
  (keyword_namespace)
  (keyword_password)
  (keyword_password_hash)
  (keyword_on_duplicate_key_update)
  (keyword_count)
  (keyword_set)
  (keyword_return)
  (keyword_overwrite)
  (keyword_throw)
  (keyword_unset)
  (keyword_always)
  (keyword_alter)
  (keyword_break)
  (keyword_continue)
  (keyword_sleep)
  (keyword_kill)
  (keyword_rebuild)
  (keyword_mtree)
  (keyword_dimension)
  (keyword_dist)
  (keyword_efc)
  (keyword_m)
  (keyword_capacity)
  (keyword_hnsw)
  (keyword_owner)
  (keyword_editor)
  (keyword_viewer)
  (keyword_duration)
  (keyword_enforced)
  (keyword_algorithm)
  (keyword_key)
  (keyword_url)
  (keyword_jwt)
  (keyword_signup)
  (keyword_issuer)
  (keyword_refresh)
  (keyword_record)
  (keyword_bearer)
  (keyword_authenticate)
  (keyword_grant)
  (keyword_access)
  (keyword_upsert)
  (keyword_replace)
  (keyword_reference)
  (keyword_computed)
] @keyword

; Declarations
(keyword_let) @keyword.storage

; Conditionals
(keyword_if) @keyword.control.conditional
(keyword_end) @keyword.control.conditional

; DEFINE FIELD ... ASSERT ...
(assert_clause
  (keyword_assert) @keyword)

; REFERENCE ON DELETE clauses:
; Keep `ON DELETE` visually grouped (same family as `NONE`/constants),
; while actions remain keyword-colored.
(reference_on_delete_clause
  (keyword_on) @constant
  (keyword_delete) @constant)

(reference_on_delete_clause
  [
    (keyword_ignore)
    (keyword_unset)
    (keyword_reject)
    (keyword_cascade)
  ] @keyword)

(reference_on_delete_clause
  (keyword_then) @keyword)
