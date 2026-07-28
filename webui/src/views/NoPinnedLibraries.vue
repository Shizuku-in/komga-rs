<template>
  <div class="pa-6">
    <empty-state
      :title="$t('no_libraries_pinned.title')"
      :sub-title="$t('no_libraries_pinned.subtitle')"
      icon="mdi-pin-off"
    />
  </div>
</template>

<script lang="ts">
import {defineComponent} from 'vue'
import EmptyState from '@/components/EmptyState.vue'

export default defineComponent({
  name: 'NoPinnedLibraries',
  components: {EmptyState},
  mounted() {
    if (this.$store.getters.getLibrariesPinned.length !== 0) {
      this.$router.push({name: 'dashboard'})
    }
  },
  watch: {
    '$store.getters.getLibrariesPinned': {
      handler(val) {
        if (val.length !== 0) this.$router.push({name: 'dashboard'})
      },
    },
  },
})
</script>

<style scoped>

</style>
