// The reference vectors of the crypto, made with the Blowfish implementation of the other downloader
// (deemix-src/packages/deemix/src/utils/blowfish.cjs), so the Rust engine can be checked against it.
//   node plugins-repo/deezer/vectors.js
// It writes src/vectors.rs (the test reads it): the stripes encrypted by the reference implementation.
const crypto = require("crypto");
const fs = require("fs");
const path = require("path");
const Blowfish = require(path.join(
  __dirname,
  "..",
  "..",
  "deemix-src",
  "packages",
  "deemix",
  "src",
  "utils",
  "blowfish.cjs"
));

const SECRET = "g4el58wc0zvf9na1";

function keyOf(id) {
  const hash = crypto.createHash("md5").update(id, "ascii").digest("hex");
  let key = "";
  for (let i = 0; i < 16; i++) {
    key += String.fromCharCode(
      hash.charCodeAt(i) ^ hash.charCodeAt(i + 16) ^ SECRET.charCodeAt(i)
    );
  }
  return { hash, key: Buffer.from(key, "binary") };
}

// The plaintext of a stripe: 2048 bytes, none of them zero (the reference strips the trailing zeros).
function stripe() {
  const plain = Buffer.alloc(2048);
  for (let i = 0; i < plain.length; i++) plain[i] = (i % 255) + 1;
  return plain;
}

const ids = ["3135556", "1"];
const lines = [
  "//! The reference vectors: the stripes encrypted by the Blowfish of the other downloader (deemix).",
  "//! Written by `vectors.js` (the script next to this file): do not edit it by hand.",
  "",
];
ids.forEach((id, index) => {
  const { hash, key } = keyOf(id);
  const cipher = new Blowfish(key, Blowfish.MODE.CBC, Blowfish.PADDING.NULL);
  cipher.setIv(Buffer.from([0, 1, 2, 3, 4, 5, 6, 7]));
  const encrypted = Buffer.from(
    cipher.encode(stripe(), Blowfish.TYPE.UINT8_ARRAY)
  );
  const name = index === 0 ? "FIRST" : "SECOND";
  lines.push(`/// The id of the track of the vector.`);
  lines.push(`pub const ${name}_ID: &str = "${id}";`);
  lines.push(`/// The MD5 of the id.`);
  lines.push(`pub const ${name}_MD5: &str = "${hash}";`);
  lines.push(`/// The key the id gives.`);
  lines.push(`pub const ${name}_KEY: &str = "${key.toString("hex")}";`);
  lines.push(`/// The stripe, encrypted.`);
  lines.push(`pub const ${name}_CIPHER: &str = "${encrypted.toString("hex")}";`);
  lines.push("");
});
fs.writeFileSync(path.join(__dirname, "src", "vectors.rs"), lines.join("\n"));
console.log("src/vectors.rs written");
