<template>
  <label class="drop-zone" v-cloak @drop.prevent="dropHandler" @dragover.prevent>
    <span class="file-input">{{ $t('common.choose_image') }}</span> - {{ $t('common.drag_drop') }}
    <input ref="input" hidden aria-hidden="true" type="file" accept="image/*" multiple @change="dropHandler">
  </label>
</template>

<script lang="ts">
import {defineComponent} from 'vue'
import {getFileFromUrl} from '@/functions/file'

export default defineComponent({
  name: 'DropZone',
  methods: {
    async dropHandler(event: Event) {
      if (event instanceof DragEvent && event.dataTransfer) {
        if (event.dataTransfer.files.length > 0)
          this.$emit('on-input-change', Array.from(event.dataTransfer.files))
        else {
          const url = event.dataTransfer.getData('text/uri-list')
          if (url) {
            const file = await getFileFromUrl(url)
            this.$emit('on-input-change', [file])
          }
        }
      }
      if (event.target instanceof HTMLInputElement && event.target.files) {
        const selectedFiles = event.target.files
        if (!selectedFiles) return

        this.$emit('on-input-change', Array.from(selectedFiles))
      }
    },
    reset() {
      (this.$refs.input as HTMLInputElement).value = ''
    },
  },
})
</script>

<style scoped>
.drop-zone {
  background: repeating-linear-gradient(
    135deg,
    color-mix(in srgb, rgb(var(--v-theme-base)) 92%, white),
    color-mix(in srgb, rgb(var(--v-theme-base)) 92%, white) 20px,
    color-mix(in srgb, rgb(var(--v-theme-base)) 92%, black) 20px,
    color-mix(in srgb, rgb(var(--v-theme-base)) 92%, black) 40px
  );
  color: rgb(var(--v-theme-contrast-light-2));
  display: block;
  font-weight: 600;
  text-align: center;
  width: 100%;
}

.file-input {
  color: rgb(var(--v-theme-info));
}

.file-input:hover {
  color: color-mix(in srgb, rgb(var(--v-theme-info)) 80%, white);
}
</style>
