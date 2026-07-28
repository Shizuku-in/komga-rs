<template>
  <div>
    <div
      ref="content"
      class="read-more__content text-body-2"
      :class="{'read-more__content--collapsed': !open}"
    >
      <slot/>
    </div>
    <v-btn v-if="expandable" variant="text" size="small" color="grey" @click="open = !open">
      {{ open ? $t(i18nLess) : $t(i18nMore) }}
      <v-icon end>mdi-chevron-{{ open ? 'up' : 'down' }}</v-icon>
    </v-btn>
  </div>
</template>

<script lang="ts">
import {defineComponent} from 'vue'

export default defineComponent({
  name: 'ReadMore',
  data: () => {
    return {
      open: false,
      expandable: false,
      resizeObserver: undefined as ResizeObserver | undefined,
    }
  },
  mounted() {
    this.resizeObserver = new ResizeObserver(this.updateOverflow)
    this.resizeObserver.observe(this.$refs.content as HTMLElement)
    this.$nextTick(this.updateOverflow)
  },
  beforeUnmount() {
    this.resizeObserver?.disconnect()
  },
  watch: {
    modelValue(val) {
      this.open = val
    },
    open(val) {
      this.$emit('update:modelValue', val)
    },
  },
  props: {
    modelValue: {
      type: Boolean,
      default: false,
    },
    i18nMore: {
      type: String,
      default: 'read_more.more',
    },
    i18nLess: {
      type: String,
      default: 'read_more.less',
    },
  },
  methods: {
    updateOverflow() {
      const content = this.$refs.content as HTMLElement
      if (!this.open) this.expandable = content.scrollHeight > content.clientHeight
    },
  },
})
</script>

<style scoped>
.read-more__content {
  white-space: pre-wrap;
}

.read-more__content--collapsed {
  display: -webkit-box;
  overflow: hidden;
  -webkit-box-orient: vertical;
  -webkit-line-clamp: 4;
}
</style>
