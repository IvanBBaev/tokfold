// A stand-in for the tokfold binary, used by harness.test.mjs: `compress ... -i
// <file>` prints a tabular frame line followed by the file unchanged.
import fs from "node:fs";

const argv = process.argv.slice(2);
const file = argv[argv.indexOf("-i") + 1];
process.stdout.write("#tkfd1 tbl\n" + fs.readFileSync(file, "utf8"));
