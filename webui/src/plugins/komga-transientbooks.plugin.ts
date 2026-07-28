import KomgaTransientBooksService from '@/services/komga-transientbooks.service'
import {AxiosInstance} from 'axios'
import type {App} from 'vue'

let service: KomgaTransientBooksService

export default {
  install (
    Vue: App,
    { http }: { http: AxiosInstance }) {
    service = new KomgaTransientBooksService(http)
    Vue.config.globalProperties.$komgaTransientBooks = service
  },
}

declare module '@vue/runtime-core' {
  interface ComponentCustomProperties {
    $komgaTransientBooks: KomgaTransientBooksService;
  }
}
