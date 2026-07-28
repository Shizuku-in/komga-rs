<template>
  <div>
    <v-list>
      <v-list-item class="contrast-1">
        <div>
          <v-list-item-title class="text-uppercase">{{ $t('common.reorder') }}</v-list-item-title>
        </div>
        <template #append>
          <v-list-item-action class="ma-0">
            <v-btn icon @click.stop.capture.prevent="dismiss">
              <v-icon>mdi-close</v-icon>
            </v-btn>
          </v-list-item-action>
        </template>
      </v-list-item>

      <v-list-item class="text-disabled">
        <template #prepend>
          <v-icon>mdi-home</v-icon>
        </template>
        <div>
          <v-list-item-title>{{ $t('navigation.home') }}</v-list-item-title>
        </div>
      </v-list-item>

      <draggable
        v-model="localItems"
        item-key="id"
        v-bind="dragOptions"
        handle=".handle"
      >
        <template #item="{ element: l }">
          <v-list-item>
            <template #prepend>
              <v-icon class="handle">mdi-drag-horizontal-variant</v-icon>
            </template>
            <div>
              <v-list-item-title class="handle">{{ l.name }}</v-list-item-title>
            </div>
            <template #append>
              <v-btn icon v-if="!l.unpinned" @click.stop.capture.prevent="unpin(l.id)" size="x-small">
                <v-icon>mdi-pin</v-icon>
              </v-btn>
              <v-btn icon v-if="l.unpinned" @click.stop.capture.prevent="pin(l.id)" size="x-small">
                <v-icon>mdi-pin-off</v-icon>
              </v-btn>
            </template>
          </v-list-item>
        </template>
      </draggable>
    </v-list>
  </div>
</template>

<script lang="ts">
import {defineComponent} from 'vue'
import draggable from 'vuedraggable'
import {LibraryDto} from '@/types/komga-libraries'
import {ClientSettingLibraryUpdate} from '@/types/komga-clientsettings'

export default defineComponent({
  name: 'ReorderLibraries',
  components: {draggable},
  data: () => {
    return {
      localItems: [] as LibraryDto[],
      unwatch: false,
    }
  },
  computed: {
    dragOptions(): any {
      return {
        animation: 200,
        ghostClass: 'ghost',
      }
    },
  },
  mounted() {
    this.localItems = this.$store.getters.getLibraries
  },
  watch: {
    localItems: {
      handler(val: LibraryDto[]) {
        const newSettings = val.map((it, index) => ({
          libraryId: it.id,
          patch: {
            order: index,
          },
        } as ClientSettingLibraryUpdate))

        this.$store.dispatch('updateLibrariesSettings', newSettings)
      },
      immediate: true,
    },
  },
  methods: {
    dismiss() {
      this.$emit('dismiss')
    },
    unpin(libraryId: string) {
      this.$store.dispatch('updateLibrarySetting', {
        libraryId: libraryId,
        patch: {
          unpinned: true,
        },
      } as ClientSettingLibraryUpdate)
      this.localItems.find(it => it.id == libraryId).unpinned = true
    },
    pin(libraryId: string) {
      this.$store.dispatch('updateLibrarySetting', {
        libraryId: libraryId,
        patch: {
          unpinned: false,
        },
      } as ClientSettingLibraryUpdate)
      this.localItems.find(it => it.id == libraryId).unpinned = false
    },
  },
})
</script>

<style scoped>
.handle {
  cursor: grab !important;
}

.ghost {
  opacity: 0.5;
  background: #c8ebfb;
}
</style>
