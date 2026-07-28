import {AxiosInstance} from 'axios'
import type {App} from 'vue'
import KomgaFontsService from '@/services/komga-fonts.service'

export default {
  install(
    Vue: App,
    {http}: { http: AxiosInstance }) {
    Vue.config.globalProperties.$komgaFonts = new KomgaFontsService(http)
  },
}

declare module '@vue/runtime-core' {
  interface ComponentCustomProperties {
    $komgaFonts: KomgaFontsService;
  }
}
