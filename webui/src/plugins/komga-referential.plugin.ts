import KomgaReferentialService from '@/services/komga-referential.service'
import { AxiosInstance } from 'axios'
import type {App} from 'vue'

export default {
  install (
    Vue: App,
    { http }: { http: AxiosInstance }) {
    Vue.config.globalProperties.$komgaReferential = new KomgaReferentialService(http)
  },
}

declare module '@vue/runtime-core' {
  interface ComponentCustomProperties {
    $komgaReferential: KomgaReferentialService;
  }
}
