// @ts-nocheck -- run with: node --experimental-strip-types --test src/lib/contacts.test.ts
// Only the pure helpers are imported (the store module needs the Svelte compiler), so they are re-declared here via dynamic import of the model file.
import { test } from "node:test";
import assert from "node:assert/strict";
import { splitList, mailbox, mergeSuggestions } from "./contactsmodel.ts";

test("splitList splits on commas, semicolons and new lines", () => {
  assert.deepEqual(splitList(" a@x.test, b@x.test;\nc@x.test ,, "), ["a@x.test", "b@x.test", "c@x.test"]);
  assert.deepEqual(splitList(""), []);
});

test("mailbox quotes names that need it and drops line breaks", () => {
  assert.equal(mailbox(null, "a@x.test"), "a@x.test");
  assert.equal(mailbox("Ada Lovelace", "a@x.test"), "Ada Lovelace <a@x.test>");
  assert.equal(mailbox("Doe, John", "j@x.test"), '"Doe, John" <j@x.test>');
  assert.equal(mailbox('Q "the" man', "q@x.test"), '"Q \\"the\\" man" <q@x.test>');
  assert.equal(mailbox("Bad\r\nName", "b@x.test"), "Bad Name <b@x.test>");
});

test("mergeSuggestions puts groups first, then contacts, then mail history, without duplicate addresses", () => {
  const out = mergeSuggestions(
    { groups: [{ name: "Team", members: [{ name: "A", address: "a@x.test" }, { name: "B", address: "b@x.test" }] }], contacts: [{ name: "A", address: "a@x.test" }, { name: "C", address: "c@x.test" }] },
    [{ name: null, address: "C@x.test" }, { name: "D", address: "d@x.test" }],
    8,
  );
  assert.deepEqual(out.map((s) => (s.group ? `group:${s.group}` : s.address)), ["group:Team", "a@x.test", "c@x.test", "d@x.test"]);
  assert.equal(out[0].members.length, 2);
  assert.equal(mergeSuggestions({ groups: [], contacts: [] }, [], 8).length, 0);
  assert.equal(mergeSuggestions({ groups: [], contacts: [] }, Array.from({ length: 20 }, (_, i) => ({ name: null, address: `u${i}@x.test` })), 5).length, 5);
});
