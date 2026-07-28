import {unref} from 'vue'

export function createVuetifyCompat(vuetify: any) {
  const breakpoint = new Proxy({}, {
    get(_target, property) {
      const display = vuetify.display as Record<PropertyKey, unknown>
      if (property === 'xsOnly') return unref(display.xs)
      return unref(display[property])
    },
  })

  const theme = {
    get dark() {
      return vuetify.theme.global.name.value === 'dark'
    },
    set dark(value: boolean) {
      vuetify.theme.change(value ? 'dark' : 'light')
    },
    get currentTheme() {
      return vuetify.theme.current.value.colors
    },
  }

  let rtl = false

  return {
    breakpoint,
    theme,
    get rtl() {
      return rtl
    },
    set rtl(value: boolean) {
      rtl = value
      document.documentElement.dir = value ? 'rtl' : 'ltr'
    },
    goTo(target: string | number, options: {offset?: number} = {}) {
      const element = typeof target === 'string' ? document.querySelector(target) : undefined
      const top = typeof target === 'number'
        ? target
        : element instanceof HTMLElement
          ? element.getBoundingClientRect().top + window.scrollY
          : 0
      window.scrollTo({top: top + (options.offset ?? 0), behavior: 'smooth'})
    },
  }
}
