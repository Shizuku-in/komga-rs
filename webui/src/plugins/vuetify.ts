import '@mdi/font/css/materialdesignicons.css'
import 'typeface-roboto/index.css'
import 'vuetify/styles'
import {createVuetify} from 'vuetify'
import * as components from 'vuetify/components'
import * as directives from 'vuetify/directives'
import {aliases, mdi} from 'vuetify/iconsets/mdi'
import IconFormatLineSpacingDown from '@/components/icons/IconFormatLineSpacingDown.vue'

export default createVuetify({
  components,
  directives,
  defaults: {
    VAutocomplete: {itemTitle: 'text', itemValue: 'value'},
    VCombobox: {itemTitle: 'text', itemValue: 'value'},
    VSelect: {itemTitle: 'text', itemValue: 'value'},
  },
  icons: {
    defaultSet: 'mdi',
    aliases: {
      ...aliases,
      formatLineSpacingDown: {
        component: IconFormatLineSpacingDown,
      },
    },
    sets: {mdi},
  },
  theme: {
    themes: {
      light: {
        dark: false,
        colors: {
          base: '#ffffff',
          primary: '#005ed3',
          secondary: '#fec000',
          accent: '#ff0335',
          'contrast-1': '#f5f5f5',
          'contrast-light-2': '#616161',
          diff: '#c8e6c9',
        },
      },
      dark: {
        dark: true,
        colors: {
          base: '#000000',
          primary: '#78baec',
          secondary: '#fec000',
          accent: '#ff0335',
          'contrast-1': '#212121',
          'contrast-light-2': '#e0e0e0',
          diff: '#1b5e20',
        },
      },
    },
  },
})
