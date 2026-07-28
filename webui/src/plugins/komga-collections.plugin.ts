import KomgaCollectionsService from '@/services/komga-collections.service'
import { AxiosInstance } from 'axios'
import type {App} from 'vue'

export default {
  install (
    Vue: App,
    { http }: { http: AxiosInstance }) {
    Vue.config.globalProperties.$komgaCollections = new KomgaCollectionsService(http)
  },
}

declare module '@vue/runtime-core' {
  interface ComponentCustomProperties {
    $komgaCollections: KomgaCollectionsService;
  }
}
