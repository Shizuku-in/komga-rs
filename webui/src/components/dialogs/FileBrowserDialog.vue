<template>
  <v-dialog v-model="modalFileBrowser"
            max-width="450"
            scrollable
  >
    <v-card>

      <v-card-title>{{ dialogTitle || $t('dialog.file_browser.dialog_title_default') }}</v-card-title>

      <v-card-text style="height: 450px">
        <v-text-field
          v-model="selectedPath"
          readonly
        />

        <v-list elevation="3" density="compact">

          <template v-if="directoryListing.hasOwnProperty('parent')">
            <v-list-item
              @click.prevent="selectParent(directoryListing.parent)"
            >
              <template #prepend>
                <v-icon>mdi-arrow-left</v-icon>
              </template>

              <div>
                <v-list-item-title>{{ $t('dialog.file_browser.parent_directory') }}</v-list-item-title>
              </div>
            </v-list-item>
            <v-divider/>
          </template>

          <div v-for="(d, index) in directoryListing.directories" :key="index">
            <v-list-item
              @click.prevent="select(d)"
            >
              <template #prepend>
                <v-icon>{{ d.type === 'directory' ? 'mdi-folder' : 'mdi-file' }}</v-icon>
              </template>

              <div>
                <v-list-item-title>
                  {{ d.name }}
                </v-list-item-title>
              </div>
            </v-list-item>

            <v-divider v-if="index !== directoryListing.directories.length-1"/>
          </div>

          <div v-for="(d, index) in directoryListing.files" :key="index">
            <v-list-item
              @click.prevent="select(d)"
            >
              <template #prepend>
                <v-icon>{{ d.type === 'directory' ? 'mdi-folder' : 'mdi-file' }}</v-icon>
              </template>

              <div>
                <v-list-item-title>
                  {{ d.name }}
                </v-list-item-title>
              </div>
            </v-list-item>

            <v-divider v-if="index !== directoryListing.files.length-1"/>
          </div>
        </v-list>
      </v-card-text>

      <v-card-actions>
        <v-spacer/>
        <v-btn variant="text" @click="dialogCancel">{{ $t('dialog.file_browser.button_cancel') }}</v-btn>
        <v-btn color="primary"
               @click="dialogConfirm"
               :disabled="!selectedPath"
        >{{ confirmText || $t('dialog.file_browser.button_confirm_default') }}
        </v-btn>
      </v-card-actions>
    </v-card>
  </v-dialog>
</template>

<script lang="ts">
import {defineComponent} from 'vue'
import {ERROR} from '@/types/events'

export default defineComponent({
  name: 'FileBrowserDialog',
  data: () => {
    return {
      directoryListing: {} as DirectoryListingDto,
      selectedPath: '',
      modalFileBrowser: false,
    }
  },
  watch: {
    modelValue(val) {
      if (val) this.dialogInit()
      this.modalFileBrowser = val
    },
    modalFileBrowser(val) {
      !val && this.dialogCancel()
    },
  },
  props: {
    modelValue: Boolean,
    path: {
      type: String,
      required: false,
    },
    showFiles: {
      type: Boolean,
      default: false,
    },
    dialogTitle: String,
    confirmText: String,
  },
  methods: {
    dialogInit() {
      try {
        this.getDirs(this.path)
        this.selectedPath = this.path
      } catch (e) {
        this.getDirs()
      }
    },
    dialogCancel() {
      this.$emit('update:modelValue', false)
    },
    dialogConfirm() {
      this.$emit('update:modelValue', false)
      this.$emit('update:path', this.selectedPath)
      this.$emit('confirm')
    },
    async getDirs(path?: string) {
      try {
        this.directoryListing = await this.$komgaFileSystem.getDirectoryListing(path, this.showFiles)
      } catch (e) {
        this.$eventHub.$emit(ERROR, {message: e.message} as ErrorEvent)
      }
    },
    selectParent(path: string) {
      this.selectedPath = path
      this.getDirs(path)
    },
    select(path: PathDto) {
      this.selectedPath = path.path
      if(path.type == 'directory') this.getDirs(path.path)
    },
  },
})
</script>

<style scoped>

</style>
