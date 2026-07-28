import KomgaFilesystemService from '@/services/komga-filesystem.service'
import { AxiosInstance } from 'axios'
import type {App} from 'vue'

export default {
  install (
    Vue: App,
    { http }: { http: AxiosInstance }) {
    Vue.config.globalProperties.$komgaFileSystem = new KomgaFilesystemService(http)
  },
}

declare module '@vue/runtime-core' {
  interface ComponentCustomProperties {
    $komgaFileSystem: KomgaFilesystemService;
  }
}
