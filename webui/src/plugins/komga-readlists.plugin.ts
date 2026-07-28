import { AxiosInstance } from 'axios'
import type {App} from 'vue'
import KomgaReadListsService from '@/services/komga-readlists.service'

export default {
  install (
    Vue: App,
    { http }: { http: AxiosInstance }) {
    Vue.config.globalProperties.$komgaReadLists = new KomgaReadListsService(http)
  },
}

declare module '@vue/runtime-core' {
  interface ComponentCustomProperties {
    $komgaReadLists: KomgaReadListsService;
  }
}
