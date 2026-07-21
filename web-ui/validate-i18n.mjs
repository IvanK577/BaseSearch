import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import ts from "typescript";

const root = path.dirname(fileURLToPath(import.meta.url));
const sourceRoot = path.join(root, "src");
const i18nPath = path.join(sourceRoot, "lib", "i18n.ts");
const expectedLabels = new Map([
  ["en", "English"],
  ["ua", "Українська"],
  ["de", "Deutsch"],
  ["es", "Español"],
  ["fr", "Français"],
  ["pl", "Polski"],
  ["pt", "Português"],
  ["ro", "Română"],
  ["hu", "Magyar"],
  ["bg", "Български"],
  ["zh", "中文"],
]);
const forbiddenCodes = new Set(["ru", "be"]);
const mojibakeMarkers = [
  "\uFFFD",
  "Ã",
  "Â",
  "â€",
  "â†",
  "ðŸ",
  "ï¿½",
  "ä¸",
  "ä¹",
  "äº",
  "æ–",
  "æœ",
  "å…",
  "çš",
  "è¯",
  "é—",
  "Ђ",
  "Ѓ",
  "Ѕ",
  "Ј",
  "Љ",
  "Њ",
  "Ћ",
  "ђ",
  "ѓ",
  "ѕ",
  "ј",
  "љ",
  "њ",
  "ћ",
];

const errors = [];

function unwrap(node) {
  while (
    node &&
    (ts.isAsExpression(node) ||
      ts.isSatisfiesExpression(node) ||
      ts.isParenthesizedExpression(node))
  ) {
    node = node.expression;
  }
  return node;
}

function propertyName(node, sourceFile) {
  if (ts.isIdentifier(node) || ts.isStringLiteralLike(node)) return node.text;
  return node.getText(sourceFile);
}

function lineOf(node, sourceFile) {
  return sourceFile.getLineAndCharacterOfPosition(node.getStart(sourceFile)).line + 1;
}

function readUtf8(file) {
  const bytes = fs.readFileSync(file);
  try {
    return new TextDecoder("utf-8", { fatal: true }).decode(bytes);
  } catch {
    errors.push(`${path.relative(root, file)} is not valid UTF-8`);
    return bytes.toString("utf8");
  }
}

function collectObject(sourceFile, variableName) {
  for (const statement of sourceFile.statements) {
    if (!ts.isVariableStatement(statement)) continue;
    for (const declaration of statement.declarationList.declarations) {
      if (!ts.isIdentifier(declaration.name) || declaration.name.text !== variableName) continue;
      const initializer = unwrap(declaration.initializer);
      if (!initializer || !ts.isObjectLiteralExpression(initializer)) return undefined;
      return initializer;
    }
  }
  return undefined;
}

function collectStringEntries(sourceFile, variableName) {
  const object = collectObject(sourceFile, variableName);
  if (!object) {
    errors.push(`missing ${variableName} dictionary`);
    return new Map();
  }

  const entries = new Map();
  for (const property of object.properties) {
    if (!ts.isPropertyAssignment(property)) continue;
    const key = propertyName(property.name, sourceFile);
    const value = unwrap(property.initializer);
    if (!ts.isStringLiteralLike(value)) continue;
    if (entries.has(key)) {
      errors.push(`${variableName}.${key} is duplicated at line ${lineOf(property, sourceFile)}`);
    }
    entries.set(key, value.text);
  }
  return entries;
}

function collectLangCodes(sourceFile) {
  for (const statement of sourceFile.statements) {
    if (!ts.isTypeAliasDeclaration(statement) || statement.name.text !== "Lang") continue;
    if (!ts.isUnionTypeNode(statement.type)) return [];
    return statement.type.types
      .filter((type) => ts.isLiteralTypeNode(type) && ts.isStringLiteral(type.literal))
      .map((type) => type.literal.text);
  }
  errors.push("missing Lang type");
  return [];
}

function collectLanguageOptions(sourceFile) {
  for (const statement of sourceFile.statements) {
    if (!ts.isVariableStatement(statement)) continue;
    for (const declaration of statement.declarationList.declarations) {
      if (!ts.isIdentifier(declaration.name) || declaration.name.text !== "LANGUAGES") continue;
      const array = unwrap(declaration.initializer);
      if (!array || !ts.isArrayLiteralExpression(array)) return [];
      return array.elements.flatMap((element) => {
        const object = unwrap(element);
        if (!ts.isObjectLiteralExpression(object)) return [];
        const option = {};
        for (const property of object.properties) {
          if (!ts.isPropertyAssignment(property)) continue;
          const value = unwrap(property.initializer);
          if (ts.isStringLiteralLike(value)) option[propertyName(property.name, sourceFile)] = value.text;
        }
        return typeof option.code === "string" && typeof option.label === "string" ? [option] : [];
      });
    }
  }
  errors.push("missing LANGUAGES options");
  return [];
}

function collectDictCodes(sourceFile) {
  const object = collectObject(sourceFile, "DICTS");
  if (!object) {
    errors.push("missing DICTS registry");
    return [];
  }
  return object.properties.flatMap((property) => {
    if (ts.isShorthandPropertyAssignment(property)) return [property.name.text];
    if (ts.isPropertyAssignment(property)) return [propertyName(property.name, sourceFile)];
    return [];
  });
}

function duplicates(values) {
  return [...new Set(values.filter((value, index) => values.indexOf(value) !== index))].sort();
}

function placeholders(value) {
  return [...value.matchAll(/\{([A-Za-z0-9_]+)\}/g)].map((match) => match[1]).sort();
}

function compareCodeSets(name, actual, expected) {
  const missing = expected.filter((code) => !actual.includes(code));
  const extra = actual.filter((code) => !expected.includes(code));
  const repeated = duplicates(actual);
  if (missing.length) errors.push(`${name} is missing languages: ${missing.join(", ")}`);
  if (extra.length) errors.push(`${name} has unsupported languages: ${extra.join(", ")}`);
  if (repeated.length) errors.push(`${name} repeats languages: ${repeated.join(", ")}`);
}

function sourceFiles(directory) {
  return fs.readdirSync(directory, { withFileTypes: true }).flatMap((entry) => {
    const target = path.join(directory, entry.name);
    if (entry.isDirectory()) return sourceFiles(target);
    return /\.[jt]sx?$/.test(entry.name) ? [target] : [];
  });
}

const sourceText = readUtf8(i18nPath);
if (sourceText !== sourceText.normalize("NFC")) errors.push("src/lib/i18n.ts is not NFC-normalized");
for (const marker of mojibakeMarkers) {
  if (sourceText.includes(marker)) errors.push(`src/lib/i18n.ts contains mojibake marker ${JSON.stringify(marker)}`);
}

const sourceFile = ts.createSourceFile(
  i18nPath,
  sourceText,
  ts.ScriptTarget.Latest,
  true,
  ts.ScriptKind.TS,
);
for (const diagnostic of sourceFile.parseDiagnostics) {
  errors.push(`i18n parse error: ${ts.flattenDiagnosticMessageText(diagnostic.messageText, " ")}`);
}

const langCodes = collectLangCodes(sourceFile);
const options = collectLanguageOptions(sourceFile);
const optionCodes = options.map(({ code }) => code);
const dictCodes = collectDictCodes(sourceFile);
const expectedCodes = [...expectedLabels.keys()];
compareCodeSets("Lang", langCodes, expectedCodes);
compareCodeSets("LANGUAGES", optionCodes, expectedCodes);
compareCodeSets("DICTS", dictCodes, expectedCodes);

for (const code of forbiddenCodes) {
  if (langCodes.includes(code) || optionCodes.includes(code) || dictCodes.includes(code)) {
    errors.push(`forbidden language ${code} is selectable or registered`);
  }
}
for (const { code, label } of options) {
  const expected = expectedLabels.get(code);
  if (expected && label !== expected) {
    errors.push(`LANGUAGES.${code} label must be ${JSON.stringify(expected)}, got ${JSON.stringify(label)}`);
  }
}

const english = collectStringEntries(sourceFile, "en");
if (!english.size) errors.push("English fallback is empty");
for (const [key, value] of english) {
  if (!value.trim()) errors.push(`en.${key} is empty`);
}

for (const code of expectedCodes) {
  const dictionary = collectStringEntries(sourceFile, code);
  const missing = [...english.keys()].filter((key) => !dictionary.has(key)).sort();
  const unknown = [...dictionary.keys()].filter((key) => !english.has(key)).sort();
  if (missing.length) errors.push(`${code} is missing ${missing.length} keys: ${missing.join(", ")}`);
  if (unknown.length) errors.push(`${code} has unknown keys: ${unknown.join(", ")}`);
  for (const [key, value] of dictionary) {
    if (!value.trim()) errors.push(`${code}.${key} is empty`);
    if (value !== value.normalize("NFC")) errors.push(`${code}.${key} is not NFC-normalized`);
    const expectedPlaceholders = placeholders(english.get(key) ?? "");
    const actualPlaceholders = placeholders(value);
    if (expectedPlaceholders.join("\0") !== actualPlaceholders.join("\0")) {
      errors.push(
        `${code}.${key} placeholders differ: expected ${expectedPlaceholders.join(", ") || "none"}; got ${actualPlaceholders.join(", ") || "none"}`,
      );
    }
  }
}

for (const file of sourceFiles(sourceRoot)) {
  if (path.resolve(file) === path.resolve(i18nPath)) continue;
  const text = readUtf8(file);
  const relative = path.relative(root, file).replaceAll("\\", "/");
  if (text !== text.normalize("NFC")) errors.push(`${relative} is not NFC-normalized`);
  for (const marker of mojibakeMarkers) {
    if (text.includes(marker)) {
      errors.push(`${relative} contains mojibake marker ${JSON.stringify(marker)}`);
    }
  }
  const parsed = ts.createSourceFile(
    file,
    text,
    ts.ScriptTarget.Latest,
    true,
    file.endsWith("x") ? ts.ScriptKind.TSX : ts.ScriptKind.TS,
  );
  function visit(node) {
    if (
      ts.isCallExpression(node) &&
      ts.isIdentifier(node.expression) &&
      node.expression.text === "t" &&
      node.arguments.length > 0 &&
      ts.isStringLiteralLike(node.arguments[0])
    ) {
      const key = node.arguments[0].text;
      if (!english.has(key)) {
        errors.push(`${relative}:${lineOf(node, parsed)} uses missing English key ${key}`);
      }
    }
    ts.forEachChild(node, visit);
  }
  visit(parsed);
}

if (errors.length) {
  console.error(`i18n validation failed (${errors.length}):`);
  for (const error of errors) console.error(`- ${error}`);
  process.exitCode = 1;
} else {
  console.log(`i18n validation passed: ${expectedCodes.length} languages, ${english.size} keys each`);
}
