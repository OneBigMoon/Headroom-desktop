import { describe, expect, it } from "vitest";

import { localeDictionaries } from "./i18n";

const { communityCopy, english, supplementalCopy, supplementalEnglish, translations } =
  localeDictionaries;

// `en` is the source of truth; every other locale is a translation of it.
const TRANSLATED_LOCALES = ["zh-CN", "zh-TW", "ja", "ko"] as const;

// A set, not a list: repeating `{name}` in a sentence is fine, dropping or
// renaming it is not.
function placeholders(value: string): string[] {
  return [...new Set([...value.matchAll(/\{(\w+)\}/g)].map((match) => match[1]))].sort();
}

function missingKeys(
  reference: Record<string, string>,
  candidate: Record<string, string | undefined>
): string[] {
  return Object.keys(reference).filter((key) => candidate[key] === undefined);
}

function extraKeys(
  reference: Record<string, string>,
  candidate: Record<string, string | undefined>
): string[] {
  return Object.keys(candidate).filter((key) => reference[key] === undefined);
}

// Three dictionaries feed one screen: the base copy, the Community build's own
// strings, and the supplemental set. Only the base one is typed as complete, so
// the other two can lose a key without failing the build - the translated
// locale then silently renders English, which reads as a bug in the app.
describe("locale parity", () => {
  for (const locale of TRANSLATED_LOCALES) {
    it(`${locale} translates every base key in full`, () => {
      expect(missingKeys(english, translations[locale])).toEqual([]);
    });

    it(`${locale} translates every Community key in full`, () => {
      expect(missingKeys(communityCopy.en, communityCopy[locale])).toEqual([]);
    });

    it(`${locale} translates every supplemental key in full`, () => {
      expect(missingKeys(supplementalEnglish, supplementalCopy[locale])).toEqual([]);
    });

    it(`${locale} carries no keys English dropped`, () => {
      expect(extraKeys(english, translations[locale])).toEqual([]);
      expect(extraKeys(communityCopy.en, communityCopy[locale])).toEqual([]);
      expect(extraKeys(supplementalEnglish, supplementalCopy[locale])).toEqual([]);
    });
  }
});

// A translation that drops `{provider}` renders the sentence with a hole in it,
// and one that renames it renders the literal placeholder. Comparing against
// English is the only thing that catches either.
describe("translation placeholders", () => {
  const families = [
    { name: "base", reference: english as Record<string, string>, locales: translations as Record<string, Record<string, string>> },
    { name: "community", reference: communityCopy.en as Record<string, string>, locales: communityCopy as Record<string, Record<string, string>> },
    { name: "supplemental", reference: supplementalEnglish as Record<string, string>, locales: supplementalCopy as unknown as Record<string, Record<string, string>> },
  ];

  for (const family of families) {
    for (const locale of TRANSLATED_LOCALES) {
      it(`${locale} keeps the ${family.name} placeholders`, () => {
        const mismatched = Object.keys(family.reference).filter((key) => {
          const translated = family.locales[locale]?.[key];
          if (translated === undefined) return false;
          return (
            placeholders(translated).join(",") !== placeholders(family.reference[key]).join(",")
          );
        });
        expect(mismatched).toEqual([]);
      });
    }
  }
});
