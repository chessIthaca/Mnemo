+++
title = "Read-receipt cites with an explicit null old_string were rejected"
created = "2026-11-09"
+++

Symptom: file_edit and multi_edit fail with "ops[0] is not a valid anchor item (invalid type: null, expected a string)" on a cite sent as {from_read, lines, new_string, old_string: null, ...}. Root cause: the advertised schema lists every ops item field as required and nullable, so schema-following models send null, but EditItem.old_string in src/tool/agent/edit_ops.rs was a plain String that accepted only a missing key. Fix: deserialize it with crate::tool::null_to_default. An anchor with a null old_string and no from_read is still refused. Regression tests in src/tool/agent/file_edit.rs: from_read_cite_with_explicit_null_fields_applies and anchor_with_null_old_string_and_no_cite_is_refused.
