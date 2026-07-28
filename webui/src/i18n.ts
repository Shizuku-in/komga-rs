import {createI18n} from 'vue-i18n'

function loadLocaleMessages(): Record<string, object> {
  const locales = import.meta.glob('./locales/*.json', {
    eager: true,
    import: 'default',
  })
  const messages: Record<string, object> = {}
  Object.entries(locales).forEach(([path, value]) => {
    const matched = path.match(/\/([A-Za-z0-9-_]+)\.json$/i)
    if (matched && matched.length > 1) {
      const locale = matched[1]
      messages[locale] = value as object
    }
  })
  return messages
}

const i18n = createI18n({
  legacy: true,
  locale: import.meta.env.VITE_I18N_LOCALE || 'en',
  fallbackLocale: import.meta.env.VITE_I18N_FALLBACK_LOCALE || 'en',
  messages: loadLocaleMessages(),
  pluralizationRules: {
    /**
     * @param choice {number} a choice index given by the input to $tc: `$tc('path.to.rule', choiceIndex)`
     * @param choicesLength {number} an overall amount of available choices
     * @returns a final choice index to select plural word by
     */
    'pl': function (choice, choicesLength) {
      // brak stron
      if (choice === 0) {
        return 0
      }

      // 1 strona
      if (choice === 1) {
        return 1
      }

      const betweenTwoAndFour = ((choice % 10) >= 2 && (choice % 10) <= 4)
      const lessThanTen = choice < 10
      const moreThanTwenty = choice > 20

      // 2 strony, 3 strony, 4 strony, 22 strony ...
      if (betweenTwoAndFour && (lessThanTen || moreThanTwenty)) {
        return 2
      }

      // other cases, 5 stron, 67 stron, 259 stron and so on
      return (choicesLength < 4) ? 2 : 3
    },
  },
})

// Framework-independent modules historically consumed the Vue 2 i18n
// instance directly. Expose the same small surface over vue-i18n's composer.
const composer = i18n.global as any
const legacyI18n = i18n as typeof i18n & {
  locale: string
  t: (...args: any[]) => string
  tc: (key: string, choice: number, named?: Record<string, unknown>) => string
}
legacyI18n.t = (...args: any[]) => composer.t(...args)
legacyI18n.tc = (key, choice, named) => composer.t(key, choice, named)
Object.defineProperty(legacyI18n, 'locale', {
  get: () => composer.locale,
  set: value => {
    composer.locale = value
  },
})

export default legacyI18n
