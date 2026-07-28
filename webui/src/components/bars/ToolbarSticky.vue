<template>
  <v-toolbar flat
             :color="color"
             class="sticky-bar"
             :style="barStyle"
             :elevation="elevation"
  >
    <slot/>
  </v-toolbar>
</template>

<script lang="ts">
import {defineComponent} from 'vue'

export default defineComponent({
  name: 'ToolbarSticky',
  computed: {
    barStyle (): any {
      if (['xs', 'sm'].includes(this.$vuetify.breakpoint.name.toString())) {
        return { 'top': '56px' }
      } else {
        return { 'top': '64px' }
      }
    },
  },
  props: {
    elevation: {
      type: Number,
      default: undefined,
    },
    color: {
      type: String,
      default: 'contrast-1',
    },
  },
})
</script>

<style scoped>
.sticky-bar {
  position: -webkit-sticky;
  position: sticky;
  z-index: 2
}
</style>
